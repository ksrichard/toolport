FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM caddy:2
COPY docker/Caddyfile /etc/caddy/Caddyfile
COPY --from=web /web/dist /srv
