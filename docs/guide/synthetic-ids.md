# Synthetic IDs

Trusted Server's Synthetic ID module maintains user recognition across all browsers through first-party identifiers. 

## What are Synthetic IDs?

Synthetic IDs are deterministic, mostly unique, privacy-safe identifiers, generated on a first site visit using HMAC-based templates that allow tracking with user consent while protecting user privacy. They are passed in requests on subsequent visits and activity. Synthetic IDs are represented in an HTTP header as such: 

```http
// Header Example
X-Synthetic-Ts: 0f99d7dc67265b6e3f9c10c2bbdca5357e739538ee1ac1f9e2d1e906299b6f37
```

They are also appended to the publisher first-pary cookie as well: 

```http
// First-Party Cookie Snippet 
 vis_opt_exp_27_exclude=1; 
----> synthetic_id=0f99d7dc67265b6e3f9c10c2bbdca5357e739538ee1ac1f9e2d1e906299b6f37; 
 sharedID=235334ad-841e-42e7-a902-c0bf2a55d56d; _sharedID_cst=zix7LPQsHA%3D%3D;
```

## How They Work

### HMAC-Based Generation

Synthetic IDs use HMAC (Hash-based Message Authentication Code) to generate deterministic but privacy-safe identifiers.

```rust
// Example placeholder
synthetic_id = hmac_sha256(secret_key, template_data)
```

### Template System

Templates define how synthetic IDs are constructed from various input sources:

- User consent signals
- Domain information
- Temporal data
- Custom parameters

## Configuration

Configure synthetic ID templates in `trusted-server.toml`:

```toml
[synthetic_ids]
template = "{{domain}}-{{timestamp}}-{{consent_hash}}"
secret_key = "your-secret-key"
```

## Privacy Considerations

- IDs are only generated with explicit user consent
- No personally identifiable information (PII) is included
- Templates are configurable per-deployment
- IDs can be rotated on schedule

## Usage Example

```javascript
// Placeholder example
const syntheticId = await trustedServer.generateSyntheticId({
  domain: 'example.com',
  consent: true
});
```

## Best Practices

1. Always verify GDPR consent before generating IDs
2. Rotate secret keys periodically
3. Use appropriate template complexity for your use case
4. Monitor ID collision rates

## Next Steps

- Learn about [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)

```sequenceDiagram
    participant Browser
    participant TS as Trusted Server (Edge)
    participant KV as KV Store
    participant Obj as Object Store (S3)
    participant Partner as Partner TS Instance

    Browser->>TS: Page request
    TS->>KV: Lookup synthetic_id
    alt Cache hit
        KV-->>TS: Return user data
    else Cache miss
        TS->>Obj: Fetch from source of truth
        Obj-->>TS: Return user data
        TS->>KV: Populate cache
    end
    TS-->>Browser: Response with personalization

    Note over TS,Obj: Async sync process
    TS->>Obj: Write new/updated records
    Partner->>Obj: Poll for updates
    Partner->>KV: Update local cache
    ```