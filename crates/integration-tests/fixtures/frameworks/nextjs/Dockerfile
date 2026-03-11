# Next.js container for integration testing.
# Builds a standalone Next.js 14 app with pages, API routes, forms, and ad slots.
#
# Build:
#   docker build -t test-nextjs:latest \
#     crates/integration-tests/fixtures/frameworks/nextjs/
#
# Run manually:
#   docker build \
#     --build-arg NODE_VERSION="$(grep '^nodejs ' .tool-versions | awk '{print $2}')" \
#     -t test-nextjs:latest \
#     crates/integration-tests/fixtures/frameworks/nextjs/
#   docker run -p 3000:3000 test-nextjs:latest

# --- Build stage ---
ARG NODE_VERSION
FROM node:${NODE_VERSION}-alpine AS builder

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci

COPY next.config.mjs ./
COPY app/ ./app/

RUN npm run build

# --- Runtime stage ---
FROM node:${NODE_VERSION}-alpine AS runner

WORKDIR /app

ENV NODE_ENV=production

# Copy standalone output from builder
COPY --from=builder /app/.next/standalone ./
COPY --from=builder /app/.next/static ./.next/static

EXPOSE 3000

CMD ["node", "server.js"]
