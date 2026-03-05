# Minimal Next.js container for integration testing.
# Builds a standalone Next.js 14 app with test pages containing ad slots.
#
# Build:
#   docker build -t test-nextjs:latest \
#     crates/integration-tests/fixtures/frameworks/nextjs/
#
# Run manually:
#   docker run -p 3000:3000 test-nextjs:latest

# --- Build stage ---
FROM node:20-alpine AS builder

WORKDIR /app

COPY package.json ./
RUN npm install

COPY next.config.mjs ./
COPY app/ ./app/

RUN npm run build

# --- Runtime stage ---
FROM node:20-alpine AS runner

WORKDIR /app

ENV NODE_ENV=production

# Copy standalone output from builder
COPY --from=builder /app/.next/standalone ./
COPY --from=builder /app/.next/static ./.next/static

EXPOSE 3000

CMD ["node", "server.js"]
