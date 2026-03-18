# Minimal WordPress container for integration testing.
# Uses PHP built-in server with a simple test theme that outputs HTML
# with ad slot divs — no MySQL or full WordPress stack required.
#
# Build:
#   docker build -t test-wordpress:latest \
#     crates/integration-tests/fixtures/frameworks/wordpress/
#
# Run manually:
#   docker run -p 8080:80 test-wordpress:latest

FROM php:8.3-cli-alpine

WORKDIR /var/www/html

# Copy test theme files that simulate WordPress HTML output
COPY theme/ /var/www/html/

EXPOSE 80

# Serve with PHP built-in server
CMD ["php", "-S", "0.0.0.0:80", "-t", "/var/www/html"]
