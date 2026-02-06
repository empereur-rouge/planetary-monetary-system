// Disable TLS verification for tests (self-signed certs in Docker)
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';
