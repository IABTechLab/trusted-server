//! Producer-policy identity binding and approved-source policy acquisition.

use super::*;

// Producer policy identity.

fn index_with_policy(version: u64, sha256: &str) -> ReleaseIndex {
    let mut value = index_value();
    value["policy_version"] = json!(version);
    value["policy_sha256"] = json!(sha256);
    parse_index(&value).expect("should accept structurally valid index")
}

#[test]
fn producer_policy_accepts_current_embedded_identity() {
    let policy = policy();
    let index = index_with_policy(1, policy.sha256());

    let accepted = accept_producer_policy(
        &index,
        &source_policy(EMBEDDED_POLICY),
        &AcceptanceRules::compiled(&policy),
    )
    .expect("should accept current producer policy");

    assert_eq!(
        accepted,
        AcceptedProducerPolicy::Current,
        "should be current policy"
    );
}

#[test]
fn producer_policy_rejects_self_selected_policy() {
    let policy = policy();
    let mut shipped = embedded_policy_value();
    shipped["bounds"]["child_seconds"] = json!(86_400);
    let shipped = encode(&shipped);
    let index = index_with_policy(1, &sha256_hex(&shipped));

    assert_eq!(
        context(accept_producer_policy(
            &index,
            &source_policy(EMBEDDED_POLICY),
            &AcceptanceRules::compiled(&policy),
        )),
        ReleaseError::ProducerPolicy,
        "should reject index digest naming a policy it ships"
    );
    assert_eq!(
        context(accept_producer_policy(
            &index,
            &source_policy(&shipped),
            &AcceptanceRules::compiled(&policy),
        )),
        ReleaseError::ProducerPolicy,
        "should reject self-consistent policy that is neither current nor reviewed"
    );
}

#[test]
fn producer_policy_rejects_altered_approved_source_bytes() {
    let policy = policy();
    let index = index_with_policy(1, policy.sha256());
    let mut altered = EMBEDDED_POLICY.to_vec();
    altered.push(b'\n');

    assert_eq!(
        context(accept_producer_policy(
            &index,
            &source_policy(&altered),
            &AcceptanceRules::compiled(&policy),
        )),
        ReleaseError::ProducerPolicy,
        "should reject approved-source bytes that differ from the index identity"
    );
}

#[test]
fn producer_policy_rejects_source_commit_mismatch() {
    let policy = policy();
    let index = index_with_policy(1, policy.sha256());
    let other = ApprovedSourcePolicy::for_tests(
        ApprovedCommit::parse(OTHER_COMMIT).expect("should parse commit"),
        EMBEDDED_POLICY.to_vec(),
    );

    assert_eq!(
        context(accept_producer_policy(
            &index,
            &other,
            &AcceptanceRules::compiled(&policy),
        )),
        ReleaseError::ProducerPolicy,
        "should reject policy bytes approved for another commit"
    );
}

fn historical_bytes() -> Vec<u8> {
    let mut value = embedded_policy_value();
    value["bounds"]["child_seconds"] = json!(60);
    encode(&value)
}

#[test]
fn producer_policy_accepts_reviewed_historical_under_newer_consumer() {
    let historical = historical_bytes();
    let digest = sha256_hex(&historical);
    let allowlist = [HistoricalPolicy {
        version: 1,
        sha256: &digest,
    }];
    let newer = "f".repeat(64);
    let rules = AcceptanceRules {
        current_version: 2,
        current_sha256: &newer,
        historical: &allowlist,
    };
    let index = index_with_policy(1, &digest);

    let accepted = accept_producer_policy(&index, &source_policy(&historical), &rules)
        .expect("should accept reviewed historical v1");

    assert_eq!(
        accepted,
        AcceptedProducerPolicy::Historical {
            version: 1,
            sha256: digest.clone(),
        },
        "should report the historical identity"
    );
}

#[test]
fn producer_policy_rejects_retired_historical_identity() {
    let historical = historical_bytes();
    let digest = sha256_hex(&historical);
    let newer = "f".repeat(64);
    let rules = AcceptanceRules {
        current_version: 2,
        current_sha256: &newer,
        historical: &[],
    };
    let index = index_with_policy(1, &digest);

    assert_eq!(
        context(accept_producer_policy(
            &index,
            &source_policy(&historical),
            &rules
        )),
        ReleaseError::ProducerPolicy,
        "should reject identity retired from the allowlist"
    );
}

#[test]
fn producer_policy_rejects_allowlisted_digest_with_wrong_source() {
    let historical = historical_bytes();
    let digest = sha256_hex(&historical);
    let allowlist = [HistoricalPolicy {
        version: 1,
        sha256: &digest,
    }];
    let newer = "f".repeat(64);
    let rules = AcceptanceRules {
        current_version: 2,
        current_sha256: &newer,
        historical: &allowlist,
    };
    let index = index_with_policy(1, &digest);

    assert_eq!(
        context(accept_producer_policy(
            &index,
            &source_policy(EMBEDDED_POLICY),
            &rules
        )),
        ReleaseError::ProducerPolicy,
        "should reject allowlisted digest when approved source bytes differ"
    );
}

#[test]
fn producer_policy_rejects_v2_under_consumer_v1() {
    let policy = policy();
    let mut producer = embedded_policy_value();
    producer["policy_version"] = json!(2);
    let producer = encode(&producer);
    let index = index_with_policy(2, &sha256_hex(&producer));

    let report = accept_producer_policy(
        &index,
        &source_policy(&producer),
        &AcceptanceRules::compiled(&policy),
    )
    .expect_err("should reject producer v2");

    assert_eq!(
        *report.current_context(),
        ReleaseError::ProducerPolicy,
        "should be a producer policy rejection"
    );
    assert!(
        format!("{report:?}").contains("unsupported producer policy version 2"),
        "should explain the unsupported version: {report:?}"
    );
}

// Approved-source policy acquisition.

fn snapshot_root(contents: &[u8]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("should create snapshot root");
    let directory = root.path().join("crates/trusted-server-cli");
    fs::create_dir_all(&directory).expect("should create snapshot directories");
    fs::write(directory.join("release-policy.json"), contents).expect("should write policy");
    root
}

fn read_root(root: &Path) -> Result<ApprovedSourcePolicy, Report<ReleaseError>> {
    let root = ApprovedSourceRoot::new(commit(), root)?;
    read_approved_snapshot(&root)
}

#[test]
fn approved_snapshot_reads_only_the_fixed_policy_path() {
    let root = snapshot_root(EMBEDDED_POLICY);
    fs::write(root.path().join("other.json"), b"{}").expect("should write decoy");

    let snapshot = read_root(root.path()).expect("should read fixed policy path");

    assert_eq!(snapshot.bytes(), EMBEDDED_POLICY, "should keep exact bytes");
    assert_eq!(
        snapshot.commit(),
        &commit(),
        "should bind the approved commit"
    );
}

#[test]
fn approved_snapshot_rejects_symlinked_policy_file() {
    let root = snapshot_root(b"");
    let target = root.path().join("elsewhere.json");
    fs::write(&target, EMBEDDED_POLICY).expect("should write target");
    let policy = root
        .path()
        .join("crates/trusted-server-cli/release-policy.json");
    fs::remove_file(&policy).expect("should remove placeholder");
    symlink(&target, &policy).expect("should create leaf symlink");

    assert_eq!(
        context(read_root(root.path())),
        ReleaseError::PolicyFetch,
        "should reject a symlinked policy file"
    );
}

#[test]
fn approved_snapshot_rejects_symlinked_directory_component() {
    let root = tempfile::tempdir().expect("should create snapshot root");
    let real = snapshot_root(EMBEDDED_POLICY);
    fs::create_dir(root.path().join("crates")).expect("should create crates");
    symlink(
        real.path().join("crates/trusted-server-cli"),
        root.path().join("crates/trusted-server-cli"),
    )
    .expect("should create directory symlink");

    assert_eq!(
        context(read_root(root.path())),
        ReleaseError::PolicyFetch,
        "should reject a symlinked directory component"
    );
}

#[test]
fn approved_snapshot_rejects_directory_at_policy_path() {
    let root = snapshot_root(b"");
    let policy = root
        .path()
        .join("crates/trusted-server-cli/release-policy.json");
    fs::remove_file(&policy).expect("should remove placeholder");
    fs::create_dir(&policy).expect("should create directory at policy path");

    assert_eq!(
        context(read_root(root.path())),
        ReleaseError::PolicyFetch,
        "should reject a non-regular policy path"
    );
}

#[test]
fn approved_snapshot_rejects_oversize_and_relative_root() {
    let root = snapshot_root(&vec![b' '; 1024 * 1024 + 1]);

    assert_eq!(
        context(read_root(root.path())),
        ReleaseError::PolicyFetch,
        "should reject policy over 1 MiB"
    );
    assert!(
        ApprovedSourceRoot::new(commit(), Path::new("relative/root")).is_err(),
        "should require an absolute snapshot root"
    );
}

#[test]
fn historical_acceptance_keeps_consumer_bounds_in_force() {
    let historical = historical_bytes();
    let digest = sha256_hex(&historical);
    let allowlist = [HistoricalPolicy {
        version: 1,
        sha256: &digest,
    }];
    let policy = policy();
    let rules = AcceptanceRules {
        historical: &allowlist,
        ..AcceptanceRules::compiled(&policy)
    };
    let mut value = index_value();
    value["policy_sha256"] = json!(digest);

    let index = parse_index(&value).expect("should accept index");
    let accepted = accept_producer_policy(&index, &source_policy(&historical), &rules)
        .expect("should accept reviewed historical policy");

    assert!(
        matches!(accepted, AcceptedProducerPolicy::Historical { .. }),
        "should accept the historical identity"
    );
    assert_eq!(
        ProcessBounds::production().child_timeout,
        Duration::from_secs(120),
        "should keep the consumer's 120 s child bound, not the historical 60 s"
    );
    assert_eq!(
        embedded_policy().expect("should reparse consumer policy"),
        policy,
        "should leave the consumer policy unchanged"
    );
    value["artifacts"][0]["companions"]["sbom"]["size"] = json!(bounds::SPDX_BYTES + 1);
    assert_eq!(
        context(parse_index(&value)),
        ReleaseError::Policy,
        "should keep enforcing consumer document bounds"
    );
}

#[test]
fn producer_policy_rejects_newer_version_even_if_allowlisted() {
    let policy = policy();
    let mut producer = embedded_policy_value();
    producer["policy_version"] = json!(2);
    let producer = encode(&producer);
    let digest = sha256_hex(&producer);
    let allowlist = [HistoricalPolicy {
        version: 2,
        sha256: &digest,
    }];
    let rules = AcceptanceRules {
        historical: &allowlist,
        ..AcceptanceRules::compiled(&policy)
    };
    let index = index_with_policy(2, &digest);

    assert_eq!(
        context(accept_producer_policy(
            &index,
            &source_policy(&producer),
            &rules
        )),
        ReleaseError::ProducerPolicy,
        "should reject a producer newer than the consumer before the allowlist"
    );
}

#[test]
fn policy_client_rejects_plain_http_without_connecting() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("should bind local listener");
    listener
        .set_nonblocking(true)
        .expect("should make listener nonblocking");
    let address = listener.local_addr().expect("should read local address");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build runtime");
    let client = policy_client().expect("should build policy client");

    let result = runtime.block_on(client.get(format!("http://{address}/redirect")).send());

    assert!(result.is_err(), "should refuse a plain HTTP URL");
    assert_eq!(
        listener.accept().map_err(|error| error.kind()).err(),
        Some(std::io::ErrorKind::WouldBlock),
        "should never connect to the local listener"
    );
}

#[test]
fn policy_fetch_respects_expired_attempt_deadline() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build runtime");
    let client = policy_client().expect("should build policy client");
    let expired = AttemptDeadline::at(Instant::now());

    let result = runtime.block_on(fetch_approved_policy(&client, &commit(), &token(), expired));

    assert_eq!(
        context(result),
        ReleaseError::Timeout,
        "should time out before any request"
    );
}

#[test]
fn policy_request_targets_only_the_fixed_contents_endpoint() {
    let request = policy_request(&commit(), &token()).expect("should build request");

    assert_eq!(
        request.url().as_str(),
        format!(
            "https://api.github.com/repos/IABTechLab/trusted-server/contents/crates/trusted-server-cli/release-policy.json?ref={COMMIT}" // allow-domain: api.github.com
        ),
        "should use the fixed endpoint and approved full commit"
    );
    assert_eq!(
        request.url().as_str(),
        format!("{POLICY_CONTENTS_URL}?ref={COMMIT}"),
        "constant"
    );
    assert_eq!(request.method(), reqwest::Method::GET, "should only read");
    assert_eq!(
        request.headers()[ACCEPT],
        "application/vnd.github.raw+json",
        "should request raw media"
    );
    let authorization = &request.headers()[AUTHORIZATION];
    assert!(
        authorization.is_sensitive(),
        "should mark credential sensitive"
    );
    assert_eq!(
        authorization,
        &format!("Bearer {TOKEN}"),
        "should send explicit token"
    );
}

#[test]
fn policy_response_rejects_redirects_and_moved_origin_or_path() {
    let request_url =
        Url::parse(&format!("{POLICY_CONTENTS_URL}?ref={COMMIT}")).expect("should parse URL");
    let other_host = Url::parse(&format!(
        "https://objects.example.com/repos/IABTechLab/trusted-server/contents/crates/trusted-server-cli/release-policy.json?ref={COMMIT}"
    ))
    .expect("should parse URL");
    let other_path = Url::parse(&format!(
        "https://api.github.com/repos/IABTechLab/trusted-server/contents/README.md?ref={COMMIT}" // allow-domain: api.github.com
    ))
    .expect("should parse URL");

    for status in [
        StatusCode::MOVED_PERMANENTLY,
        StatusCode::FOUND,
        StatusCode::TEMPORARY_REDIRECT,
        StatusCode::PERMANENT_REDIRECT,
    ] {
        assert_eq!(
            context(check_policy_response(
                &request_url,
                status,
                &request_url,
                None
            )),
            ReleaseError::PolicyFetch,
            "should reject redirect status {status}"
        );
    }
    for final_url in [&other_host, &other_path] {
        assert_eq!(
            context(check_policy_response(
                &request_url,
                StatusCode::OK,
                final_url,
                None
            )),
            ReleaseError::PolicyFetch,
            "should reject changed origin or path {final_url}"
        );
    }
    assert_eq!(
        context(check_policy_response(
            &request_url,
            StatusCode::NOT_FOUND,
            &request_url,
            None
        )),
        ReleaseError::PolicyFetch,
        "should reject non-200"
    );
    assert_eq!(
        context(check_policy_response(
            &request_url,
            StatusCode::OK,
            &request_url,
            Some(bounds::POLICY_BYTES + 1),
        )),
        ReleaseError::PolicyFetch,
        "should reject declared oversize"
    );
    assert!(
        check_policy_response(&request_url, StatusCode::OK, &request_url, Some(10)).is_ok(),
        "should accept exact 200 response"
    );
}

#[test]
fn policy_body_stream_is_bounded() {
    let mut body = BoundedBody::new(bounds::POLICY_BYTES);
    let chunk = vec![b' '; 512 * 1024];

    body.push(&chunk).expect("should accept first half");
    body.push(&chunk).expect("should accept exact bound");

    assert_eq!(
        context(body.push(b"x")),
        ReleaseError::PolicyFetch,
        "should reject streamed byte past 1 MiB"
    );
}

#[test]
fn approved_source_policy_never_comes_from_index_fields() {
    let mut value = index_value();
    value["source_commit"] = json!(OTHER_COMMIT);
    value["recipe_source_commit"] = json!(OTHER_COMMIT);
    value["cohort_source_commit"] = json!(OTHER_COMMIT);

    assert_eq!(
        context(parse_index(&value)),
        ReleaseError::Policy,
        "should require independently approved source commit"
    );
}

#[test]
fn policy_client_returns_redirects_without_following() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("should bind local listener");
    let address = listener.local_addr().expect("should read local address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("should accept first request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        let response = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{address}/followed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(response.as_bytes())
            .expect("should write redirect");
        drop(stream);
        listener
            .set_nonblocking(true)
            .expect("should make listener nonblocking");
        std::thread::sleep(Duration::from_millis(300));
        listener.accept().is_ok()
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build runtime");
    let client = plain_http_policy_client_for_tests().expect("should build test client");

    let response = runtime
        .block_on(client.get(format!("http://{address}/policy")).send())
        .expect("should receive the redirect response");
    let followed = server.join().expect("should join local server");

    assert_eq!(
        response.status(),
        StatusCode::TEMPORARY_REDIRECT,
        "should return the 307 itself"
    );
    assert!(!followed, "should never request the redirect target");
}

#[test]
fn fixed_endpoints_derive_from_policy_identity() {
    assert_eq!(
        POLICY_CONTENTS_URL,
        format!(
            "https://api.{GITHUB_HOST}/repos/{REPOSITORY}/contents/crates/trusted-server-cli/release-policy.json" // allow-domain: api
        ),
        "should fetch from the policy repository"
    );
    assert_eq!(
        SIGNER_WORKFLOW_PREFIX,
        format!(
            "https://{GITHUB_HOST}/{REPOSITORY}/.github/workflows/release-build.yml@refs/tags/"
        ),
        "should sign from the policy repository"
    );
}
