import { describe, expect, it } from 'vitest';

import { validateTraceRequestContext } from '../../src/trace/context';

function context() {
  return {
    schema_version: 1,
    captured_at: '2026-10-05T10:15:30.123Z',
    network: {},
    cookies: {
      ts_ec: { source: 'request', state: 'absent' },
      ts_eids: { source: 'request', state: 'absent' },
      ts_tester: { source: 'request', state: 'absent' },
      diagnostics_session: { source: 'request', state: 'absent' },
    },
  };
}

describe('redacted trace request context validation', () => {
  it('accepts missing optional platform facts and all specified cookie states', () => {
    expect(validateTraceRequestContext(context())).toBe(true);
    const value = context();
    Object.assign(value.cookies, {
      ts_ec: { source: 'request', state: 'present_valid', detail: 'valid_ec_format' },
      ts_eids: { source: 'request', state: 'present_invalid', detail: 'malformed' },
      ts_tester: { source: 'request', state: 'duplicate', detail: 'multiple_values' },
      diagnostics_session: {
        source: 'request',
        state: 'unavailable',
        detail: 'runtime_header_ambiguous',
      },
    });
    expect(validateTraceRequestContext(value)).toBe(true);
  });

  it.each(['192.0.2.0/24', '2001:db8:1234::/48', '::/48'])(
    'accepts the display-only masked prefix %s',
    (masked_client_ip) => {
      const value = context();
      Object.assign(value.network, { masked_client_ip });
      expect(validateTraceRequestContext(value)).toBe(true);
    }
  );

  it.each([
    '192.0.2.129',
    '192.0.2.129/24',
    '192.0.2.0/32',
    '256.0.2.0/24',
    '01.0.2.0/24',
    '2001:db8:1234:5678::1',
    '2001:db8:1234:1::/48',
    'example.com',
  ])('rejects a full address or incorrectly masked prefix %s', (masked_client_ip) => {
    const value = context();
    Object.assign(value.network, { masked_client_ip });
    expect(validateTraceRequestContext(value)).toBe(false);
  });

  it.each([
    '2026-02-30T10:00:00Z',
    '2026-10-05T25:00:00Z',
    '2026-10-05T10:00:00+00:00',
    '2026-10-05',
    '2026-10-05T10:00:60Z',
    '2026-10-05t10:00:00z',
    '2026-10-05T10:15:30Z\n',
    '2026-10-05T10:15:30Z\r',
    '2026-10-05T10:15:30Z\u2028',
    '2026-10-05T10:15:30Z\u2029',
  ])('rejects an invalid or noncanonical UTC capture time %s', (captured_at) => {
    expect(validateTraceRequestContext({ ...context(), captured_at })).toBe(false);
  });

  it.each([
    { country: 'USA' },
    { country: 'é' },
    { asn: -1 },
    { asn: 4_294_967_296 },
    { asn: 1.5 },
    { region: 'é'.repeat(17) },
    { tls_cipher: 'x'.repeat(33) },
    { edge_hostname: 'x'.repeat(129) },
    { region: 'a\n' },
    { region: 'a\u0085' },
    { region: 'a\u202e' },
    { region: '\ud800' },
    { ja4: 'secret-fingerprint' },
    { full_client_ip: '192.0.2.129' },
  ])('rejects forbidden or invalid platform facts %j', (network) => {
    expect(validateTraceRequestContext({ ...context(), network })).toBe(false);
  });

  it.each([
    { state: 'absent', detail: 'malformed' },
    { state: 'present_valid', detail: 'valid_ec_format' },
    { state: 'present_invalid', detail: 'runtime_header_ambiguous' },
    { state: 'unavailable', detail: 'multiple_values' },
    { state: 'duplicate', detail: 'header_too_large' },
    { state: 'absent', value: 'raw-cookie-sentinel' },
    { state: 'unavailable', detail: 'unknown_reason' },
    { state: 'present_valid' },
  ])('rejects invalid session state/detail pairs and added values %j', (health) => {
    const value = context();
    Object.assign(value.cookies, { diagnostics_session: { source: 'request', ...health } });
    expect(validateTraceRequestContext(value)).toBe(false);
  });

  it('requires exactly the four owned cookie names and rejects extra envelope fields', () => {
    const value = context();
    expect(validateTraceRequestContext({ ...value, path: '/secret-sentinel' })).toBe(false);
    expect(validateTraceRequestContext({ ...value, schema_version: 2 })).toBe(false);
    expect(
      validateTraceRequestContext({ ...value, cookies: { ...value.cookies, other: {} } })
    ).toBe(false);
    expect(validateTraceRequestContext({ ...value, cookies: {} })).toBe(false);
  });

  it.each([
    ['ts_ec', 'valid_ec_format'],
    ['ts_eids', 'valid_eids_format'],
    ['ts_tester', 'valid_tester_value'],
    ['diagnostics_session', 'valid_diagnostics_value'],
  ])('binds the valid detail to its exact owned cookie %s', (name, detail) => {
    const value = context();
    Object.assign(value.cookies, { [name]: { source: 'request', state: 'present_valid', detail } });
    expect(validateTraceRequestContext(value)).toBe(true);
    Object.assign(value.cookies, {
      [name]: {
        source: 'request',
        state: 'present_valid',
        detail: detail === 'valid_ec_format' ? 'valid_diagnostics_value' : 'valid_ec_format',
      },
    });
    expect(validateTraceRequestContext(value)).toBe(false);
  });

  it.each(['header_too_large', 'header_not_utf8', 'runtime_header_ambiguous'])(
    'preserves the aggregate unavailable reason on every owned cookie: %s',
    (detail) => {
      const value = context();
      for (const name of Object.keys(value.cookies))
        Object.assign(value.cookies, {
          [name]: { source: 'request', state: 'unavailable', detail },
        });
      expect(validateTraceRequestContext(value)).toBe(true);
    }
  );

  it('never coerces objects into a permitted detail or invokes getters', () => {
    const value = context();
    Object.assign(value.cookies, {
      diagnostics_session: {
        source: 'request',
        state: 'unavailable',
        detail: { toString: () => 'runtime_header_ambiguous' },
      },
    });
    expect(validateTraceRequestContext(value)).toBe(false);
    expect(
      validateTraceRequestContext(
        Object.defineProperty(context(), 'network', {
          enumerable: true,
          get: () => {
            throw new Error('should not invoke getters');
          },
        })
      )
    ).toBe(false);
    expect(
      validateTraceRequestContext(
        new Proxy(
          {},
          {
            getPrototypeOf: () => {
              throw new Error('should fail closed');
            },
          }
        )
      )
    ).toBe(false);
  });

  it('preserves valid boundary-sized Unicode and unsigned ASN facts', () => {
    const value = context();
    Object.assign(value.network, {
      country: 'US',
      region: 'é'.repeat(16),
      edge_hostname: 'x'.repeat(128),
      edge_region: 'x'.repeat(128),
      edge_pop: 'x'.repeat(32),
      http_version: 'HTTP/2',
      tls_protocol: 'TLSv1.3',
      tls_cipher: 'example-cipher',
      asn: 4_294_967_295,
    });
    expect(validateTraceRequestContext(value)).toBe(true);
    expect(validateTraceRequestContext({ ...value, captured_at: '2024-02-29T00:00:00Z' })).toBe(
      true
    );
    expect(validateTraceRequestContext({ ...value, captured_at: '1900-02-29T00:00:00Z' })).toBe(
      false
    );
  });
});
