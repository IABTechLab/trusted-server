# Deterministic publisher/PBS fixture for the tester-only ad trace journey.
FROM php:8.3-cli-alpine

WORKDIR /var/www/html

COPY crates/trusted-server-integration-tests/fixtures/frameworks/ad-trace/public/ /var/www/html/
COPY target/integration-test-artifacts/prebid/ /opt/prebid/

RUN bundle="$(find /opt/prebid -maxdepth 1 -name 'trusted-prebid-*.js' -type f | head -n 1)" \
    && test -n "$bundle" \
    && cp "$bundle" /var/www/html/prebid-bundle.js

EXPOSE 80

CMD ["php", "-S", "0.0.0.0:80", "router.php"]
