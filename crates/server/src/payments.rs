//! The Stripe client. ToolPort does only three things with Stripe: it starts a Checkout for the
//! first subscription, it opens the Customer Portal, and it reads the subscription that a webhook
//! names. It never changes a subscription.

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use reqwest::Url;
use stripe::{Client, ClientBuilder, StripeError};
use stripe_billing::{
    billing_portal_configuration::ListBillingPortalConfiguration,
    billing_portal_session::CreateBillingPortalSession, subscription::RetrieveSubscription,
};
use stripe_checkout::{
    CheckoutSessionMode,
    checkout_session::{
        CreateCheckoutSession, CreateCheckoutSessionLineItems,
        CreateCheckoutSessionSubscriptionData,
    },
};
use stripe_core::customer::CreateCustomer;
use stripe_product::price::ListPrice;

use crate::config::Config;

/// The metadata key that links Stripe objects to a ToolPort user.
pub const USER_ID_KEY: &str = "toolport_user_id";
/// `scripts/stripe-setup.sh` marks its Customer Portal configuration with this metadata.
const PORTAL_KEY: &str = "toolport";
const PORTAL_VALUE: &str = "portal";

#[derive(Debug, thiserror::Error)]
pub enum PaymentsError {
    #[error("the Stripe request failed")]
    Stripe(#[from] StripeError),
    #[error("Stripe has no active price with the lookup key {0}")]
    UnknownPrice(String),
    #[error("Stripe returned a checkout session without a URL")]
    NoCheckoutUrl,
}

/// The parts of a Stripe subscription that ToolPort stores.
#[derive(Debug, Clone)]
pub struct SubscriptionState {
    pub id: String,
    pub customer_id: String,
    pub status: String,
    pub lookup_key: Option<String>,
    pub period_start: Option<i64>,
    pub period_end: Option<i64>,
    pub cancel_at_period_end: bool,
    pub user_id: Option<i64>,
}

/// This type has no `Debug`, so a log line cannot print the secrets.
pub struct Payments {
    client: Client,
    webhook_secret: String,
    /// Lookup key to price id. A price id does not change, so the cache never expires.
    prices: Mutex<HashMap<String, String>>,
    portal_configuration: Mutex<Option<String>>,
}

#[derive(Debug, thiserror::Error)]
#[error("the Stripe client settings are not valid")]
pub struct PaymentsConfigError(#[source] StripeError);

impl Payments {
    /// Returns `None` when a Stripe setting is missing. Payments are then off.
    pub fn from_config(config: &Config) -> Result<Option<Self>, PaymentsConfigError> {
        let secret_key = config
            .stripe_secret_key
            .as_deref()
            .filter(|v| !v.is_empty());
        let webhook_secret = config
            .stripe_webhook_secret
            .as_deref()
            .filter(|v| !v.is_empty());
        let (Some(secret_key), Some(webhook_secret)) = (secret_key, webhook_secret) else {
            if secret_key.is_some() != webhook_secret.is_some() {
                tracing::warn!(
                    "payments are off: set both STRIPE_SECRET_KEY and STRIPE_WEBHOOK_SECRET"
                );
            }
            return Ok(None);
        };
        Self::new(secret_key, webhook_secret, config.stripe_api_url.as_ref()).map(Some)
    }

    fn new(
        secret_key: &str,
        webhook_secret: &str,
        api_url: Option<&Url>,
    ) -> Result<Self, PaymentsConfigError> {
        let mut builder = ClientBuilder::new(secret_key)
            .timeout(Duration::from_secs(10))
            .app_info("toolport", None, None);
        if let Some(url) = api_url {
            builder = builder.url(url.as_str());
        }
        Ok(Self {
            client: builder.build().map_err(PaymentsConfigError)?,
            webhook_secret: webhook_secret.to_owned(),
            prices: Mutex::new(HashMap::new()),
            portal_configuration: Mutex::new(None),
        })
    }

    pub fn webhook_secret(&self) -> &str {
        &self.webhook_secret
    }

    /// Finds the price id for a lookup key. `scripts/stripe-setup.sh` creates the prices.
    pub async fn price_id(&self, lookup_key: &str) -> Result<String, PaymentsError> {
        if let Some(id) = lock(&self.prices).get(lookup_key) {
            return Ok(id.clone());
        }
        let prices = ListPrice::new()
            .lookup_keys(vec![lookup_key.to_owned()])
            .active(true)
            .send(&self.client)
            .await?;
        let id = prices
            .data
            .into_iter()
            .next()
            .map(|price| price.id.to_string())
            .ok_or_else(|| PaymentsError::UnknownPrice(lookup_key.to_owned()))?;
        lock(&self.prices).insert(lookup_key.to_owned(), id.clone());
        Ok(id)
    }

    pub async fn create_customer(&self, user_id: i64, name: &str) -> Result<String, PaymentsError> {
        let customer = CreateCustomer::new()
            .name(name)
            .metadata(HashMap::from([(
                USER_ID_KEY.to_owned(),
                user_id.to_string(),
            )]))
            .send(&self.client)
            .await?;
        Ok(customer.id.to_string())
    }

    pub async fn checkout_url(
        &self,
        user_id: i64,
        customer_id: &str,
        price_id: &str,
        success_url: &str,
        cancel_url: &str,
    ) -> Result<String, PaymentsError> {
        let mut item = CreateCheckoutSessionLineItems::new();
        item.price = Some(price_id.to_owned());
        item.quantity = Some(1);
        let mut subscription = CreateCheckoutSessionSubscriptionData::new();
        subscription.metadata = Some(HashMap::from([(
            USER_ID_KEY.to_owned(),
            user_id.to_string(),
        )]));
        let session = CreateCheckoutSession::new()
            .mode(CheckoutSessionMode::Subscription)
            .customer(customer_id)
            .client_reference_id(user_id.to_string())
            .line_items(vec![item])
            .subscription_data(subscription)
            .success_url(success_url)
            .cancel_url(cancel_url)
            .send(&self.client)
            .await?;
        session.url.ok_or(PaymentsError::NoCheckoutUrl)
    }

    pub async fn portal_url(
        &self,
        customer_id: &str,
        return_url: &str,
    ) -> Result<String, PaymentsError> {
        let mut request = CreateBillingPortalSession::new()
            .customer(customer_id)
            .return_url(return_url);
        if let Some(configuration) = self.portal_configuration().await? {
            request = request.configuration(configuration);
        }
        Ok(request.send(&self.client).await?.url)
    }

    /// The portal configuration of `scripts/stripe-setup.sh`. Without it, Stripe uses its
    /// default configuration.
    async fn portal_configuration(&self) -> Result<Option<String>, PaymentsError> {
        if let Some(id) = lock(&self.portal_configuration).clone() {
            return Ok(Some(id));
        }
        let list = ListBillingPortalConfiguration::new()
            .active(true)
            .limit(100)
            .send(&self.client)
            .await?;
        let found = list
            .data
            .into_iter()
            .find(|config| {
                config
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get(PORTAL_KEY))
                    .is_some_and(|v| v == PORTAL_VALUE)
            })
            .map(|config| config.id.to_string());
        if let Some(id) = &found {
            *lock(&self.portal_configuration) = Some(id.clone());
        }
        Ok(found)
    }

    pub async fn subscription(&self, id: &str) -> Result<SubscriptionState, PaymentsError> {
        let sub = RetrieveSubscription::new(id).send(&self.client).await?;
        // ToolPort makes subscriptions with one item. Since API version 2025-03-31, the period
        // is on the item and not on the subscription.
        let item = sub.items.data.first();
        Ok(SubscriptionState {
            id: sub.id.to_string(),
            customer_id: sub.customer.id().to_string(),
            status: sub.status.as_str().to_owned(),
            lookup_key: item.and_then(|item| item.price.lookup_key.clone()),
            period_start: item.map(|item| item.current_period_start),
            period_end: item.map(|item| item.current_period_end),
            cancel_at_period_end: sub.cancel_at_period_end,
            user_id: sub.metadata.get(USER_ID_KEY).and_then(|v| v.parse().ok()),
        })
    }
}

/// The caches hold only ids, so a poisoned lock still has good data.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
