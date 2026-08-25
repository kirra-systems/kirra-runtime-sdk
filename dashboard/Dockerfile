# #686: digest-pinned base image (Dependabot `docker` bumps it). Tag kept for readability.
FROM --platform=$BUILDPLATFORM node:20-alpine@sha256:fb4cd12c85ee03686f6af5362a0b0d56d50c58a04632e6c0fb8363f609372293 AS builder
WORKDIR /app
COPY package.json ./
RUN npm install
COPY . .
RUN npm run build

# #686: digest-pinned base image (Dependabot `docker` bumps it).
FROM nginx:alpine@sha256:db35bfc6b2951e7f8a72db5db120288c127ffaeeb4a6d4b95a26fead017d5913
COPY --from=builder /app/dist /usr/share/nginx/html
EXPOSE 80
HEALTHCHECK --interval=10s --timeout=3s \
    CMD wget -qO- http://localhost/ || exit 1
