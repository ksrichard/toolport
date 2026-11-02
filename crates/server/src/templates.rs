//! Ready-made APIs for the onboarding. A new user picks one and gets working tools without a spec.

use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, Serialize, TS)]
#[ts(export)]
pub struct Template {
    pub id: &'static str,
    pub title: &'static str,
    /// The upstream host that the card shows.
    pub host: &'static str,
    pub summary: &'static str,
    /// The text of the small label on the card.
    pub badge: &'static str,
    /// False when the upstream API works better with a user key.
    pub keyless: bool,
    /// The default server name in the onboarding form.
    pub server_name: &'static str,
    #[serde(skip)]
    #[ts(skip)]
    pub specs: &'static [&'static str],
}

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "weather",
        title: "Weather",
        host: "api.open-meteo.com",
        summary: "Current weather for any place.",
        badge: "No key needed",
        keyless: true,
        server_name: "weather",
        // One spec per upstream host. The geocoding API finds the coordinates of a place.
        specs: &[
            include_str!("../templates/open-meteo-geocoding.json"),
            include_str!("../templates/open-meteo-forecast.json"),
        ],
    },
    Template {
        id: "github-issues",
        title: "GitHub issues",
        host: "api.github.com",
        summary: "Open issues from a repo.",
        // Upstream auth comes with the APIs page. Until then, GitHub allows 60 calls per hour.
        badge: "Public repos, 60 calls/h",
        keyless: false,
        server_name: "github-issues",
        specs: &[include_str!("../templates/github-issues.json")],
    },
    Template {
        id: "hacker-news",
        title: "Hacker News",
        host: "hacker-news.firebaseio.com",
        summary: "Top stories right now.",
        badge: "No key needed",
        keyless: true,
        server_name: "hacker-news",
        specs: &[include_str!("../templates/hacker-news.json")],
    },
];

pub fn find(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|template| template.id == id)
}

#[cfg(test)]
mod tests {
    use toolport_core::{convert, parse_spec};

    use super::*;

    #[test]
    fn every_template_spec_converts_without_skips() {
        for template in TEMPLATES {
            assert!(is_valid(template.server_name), "{}", template.id);
            for spec in template.specs {
                let conversion = convert(&parse_spec(spec).unwrap());
                assert!(!conversion.tools.is_empty(), "{}", template.id);
                assert!(
                    conversion.skipped.is_empty(),
                    "{}: {:?}",
                    template.id,
                    conversion.skipped
                );
            }
        }
    }

    fn is_valid(name: &str) -> bool {
        toolport_core::server_name::is_valid_server_name(name)
    }
}
