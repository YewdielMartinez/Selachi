use super::*;

/// (input, secret that must disappear, kind that must be reported)
const POSITIVE: &[(&str, &str, &str)] = &[
    (
        "export ANTHROPIC_API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
        "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
        "anthropic",
    ),
    (
        "curl https://api.x.com -H 'x-key: sk-ant-api03-abcdefghijklmnopqrstuvwxyz'",
        "sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
        "anthropic",
    ),
    (
        "OPENAI=sk-proj-AbCdEfGhIjKlMnOpQrSt1234567890",
        "sk-proj-AbCdEfGhIjKlMnOpQrSt1234567890",
        "openai",
    ),
    (
        "key is sk-abcdefghijklmnopqrstuvwx1234 ok",
        "sk-abcdefghijklmnopqrstuvwx1234",
        "openai",
    ),
    (
        "git clone https://x:ghp_abcdefghijklmnopqrstuvwxyz0123456789@github.com/a/b",
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "github",
    ),
    (
        "token gho_abcdefghijklmnopqrstuvwxyz0123456789 end",
        "gho_abcdefghijklmnopqrstuvwxyz0123456789",
        "github",
    ),
    (
        "use github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz0123456789ABCDEFGHIJ now",
        "github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz0123456789ABCDEFGHIJ",
        "github",
    ),
    (
        "SLACK=xoxb-123456789012-abcdefghijklmnop",
        "xoxb-123456789012-abcdefghijklmnop",
        "slack",
    ),
    (
        "id AKIAIOSFODNN7EXAMPLE here",
        "AKIAIOSFODNN7EXAMPLE",
        "aws",
    ),
    (
        "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        "secreto",
    ),
    (
        "gkey AIzaSyA-1234567890abcdefghijklmnopqrstu done",
        "AIzaSyA-1234567890abcdefghijklmnopqrstu",
        "google",
    ),
    (
        "jwt eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c ok",
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
        "jwt",
    ),
    (
        "curl -H 'Authorization: Bearer abcdef0123456789abcdef' x",
        "abcdef0123456789abcdef",
        "bearer",
    ),
    (
        "Authorization: Basic dXNlcjpwYXNzd29yZDEyMw==",
        "dXNlcjpwYXNzd29yZDEyMw==",
        "basic",
    ),
    (
        "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA1234\nabcd\n-----END RSA PRIVATE KEY-----\nafter",
        "MIIEowIBAAKCAQEA1234",
        "clave-privada",
    ),
    (
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n",
        "b3BlbnNzaC1rZXktdjEAAAAA",
        "clave-privada",
    ),
    (
        "Server=db;Database=app;User Id=sa;Password=P@ssw0rd!x;Encrypt=true",
        "P@ssw0rd!x",
        "conexion",
    ),
    (
        "postgres://admin:s3cr3tpass@localhost:5432/app",
        "s3cr3tpass",
        "url",
    ),
    ("set PASSWORD=hunter2", "hunter2", "secreto"),
    ("mycli --password hunter2 --verbose", "hunter2", "flag"),
    ("mycli --token=abc123def456", "abc123def456", "flag"),
    ("mycli -Password 'p@ss w0rd'", "p@ss w0rd", "flag"),
    ("$env:GITHUB_TOKEN = \"abc123456\"", "abc123456", "secreto"),
    (
        "{\"api_key\": \"abcd1234efgh\", \"name\": \"x\"}",
        "abcd1234efgh",
        "secreto",
    ),
    ("password: hunter22", "hunter22", "secreto"),
    ("DB_PASSWORD=letmein", "letmein", "env"),
    ("STRIPE_KEY=\"abc123xyz789\"", "abc123xyz789", "env"),
    (
        "sk_live_abcdefghijklmnop1234 and more",
        "sk_live_abcdefghijklmnop1234",
        "stripe",
    ),
    (
        "npm_abcdefghijklmnopqrstuvwxyz0123456789",
        "npm_abcdefghijklmnopqrstuvwxyz0123456789",
        "npm",
    ),
    (
        "hf_abcdefghijklmnopqrstuvwxyz0123456789",
        "hf_abcdefghijklmnopqrstuvwxyz0123456789",
        "huggingface",
    ),
];

/// Text that must come back untouched.
const NEGATIVE: &[&str] = &[
    // git SHAs, UUIDs, hashes
    "commit 5f94df478a1b2c3d4e5f60718293a4b5c6d7e8f9 (HEAD -> main)",
    "id 123e4567-e89b-12d3-a456-426614174000 created",
    "sha256: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    // base64 image data
    "<img src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==\">",
    // code and config that mention the words
    "cargo test token_parser",
    "pub token: String,",
    "secret: Option<String>,",
    "password: string;",
    "let token = lexer.next();",
    "let token = self.token;",
    "token = next_token",
    "max_tokens: 4096",
    "tokens = 100",
    "tokenizer=bpe",
    "const apiKey = process.env.API_KEY;",
    "password = os.environ[\"DB_PASS\"]",
    "password=${DB_PASSWORD}",
    "$env:API_KEY = $secret",
    "export PASSWORD=$1",
    "api_key=<your-key>",
    "api_key = \"{api_key}\"",
    "token: ********",
    "SSH_KEY_PATH=C:\\Users\\a\\.ssh\\id_ed25519",
    "PWD=/c/Users/venta/Desktop",
    "KEYBOARD=us",
    "KEY_REPEAT=5",
    "TOKEN_URL=https://example.com/oauth/token",
    "mycli --password-stdin",
    "mycli --token --verbose",
    "Authorization: Bearer <token>",
    "the bearer authentication scheme",
    "ls sk-short",
    "risk-assessment-framework-document-2024-final",
    "git switch -c feature/add-secret-manager",
    "cd C:\\secrets-docs",
    "npm run build -- --release",
    "Visit https://example.com/page?id=123",
    "ssh://git@github.com/owner/repo.git",
    "foo bar baz AKIA",
];

#[test]
fn secrets_are_hidden() {
    for (input, secret, kind) in POSITIVE {
        let r = redact(input);
        assert!(
            !r.text.contains(secret),
            "secret survived in {input:?} -> {:?}",
            r.text
        );
        assert!(r.count >= 1, "count for {input:?}");
        assert!(
            r.kinds.contains(kind),
            "kind {kind} missing for {input:?}: {:?}",
            r.kinds
        );
        assert!(r.text.contains("«oculto:"), "no placeholder in {input:?}");
    }
}

#[test]
fn false_positives_are_left_alone() {
    for input in NEGATIVE {
        let r = redact(input);
        assert_eq!(&r.text, input, "should not redact: {input:?}");
        assert_eq!(r.count, 0, "{input:?}");
        assert!(r.is_clean());
    }
}

#[test]
fn surrounding_text_is_preserved() {
    let r = redact("mycli --password hunter2 --verbose");
    assert_eq!(r.text, "mycli --password «oculto:flag» --verbose");
    let r = redact("Server=db;User Id=sa;Password=P@ss1;Encrypt=true");
    assert_eq!(
        r.text,
        "Server=db;User Id=sa;Password=«oculto:conexion»;Encrypt=true"
    );
    let r = redact("{\"token\": \"abc12345\"}");
    assert_eq!(r.text, "{\"token\": \"«oculto:secreto»\"}");
    let r = redact("Authorization: Bearer abcdef0123456789abcdef");
    assert_eq!(r.text, "Authorization: Bearer «oculto:bearer»");
    let r = redact("postgres://admin:s3cr3tpass@localhost/app");
    assert_eq!(r.text, "postgres://admin:«oculto:url»@localhost/app");
}

#[test]
fn multiline_env_file() {
    let env = "# config\nNODE_ENV=production\nPORT=3000\nDATABASE_PASSWORD=hunter2!\nAPI_TOKEN=\"abc123def\"\nDEBUG=true\nSECRET_KEY=s3cr3t-value-1\n";
    let r = redact(env);
    assert_eq!(r.count, 3, "{}", r.text);
    assert!(r.text.contains("NODE_ENV=production"));
    assert!(r.text.contains("PORT=3000"));
    assert!(r.text.contains("DEBUG=true"));
    assert!(!r.text.contains("hunter2"));
    assert!(!r.text.contains("abc123def"));
    assert!(!r.text.contains("s3cr3t-value-1"));
}

#[test]
fn pem_block_is_one_span() {
    let pem = "before\n-----BEGIN PRIVATE KEY-----\nAAAA\nBBBB\n-----END PRIVATE KEY-----\nafter";
    let r = redact(pem);
    assert_eq!(r.text, "before\n«oculto:clave-privada»\nafter");
    assert_eq!(r.count, 1);
}

#[test]
fn overlapping_rules_count_once() {
    // jwt + bearer + authorization all see the same token.
    let r = redact(
        "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijk",
    );
    assert_eq!(r.count, 1, "{}", r.text);
    assert_eq!(r.kinds, ["jwt"]);
}

#[test]
fn redaction_is_idempotent() {
    for (input, _, _) in POSITIVE {
        let once = redact(input);
        let twice = redact(&once.text);
        assert_eq!(once.text, twice.text, "not idempotent for {input:?}");
        assert_eq!(twice.count, 0);
    }
}

#[test]
fn counts_and_kinds_for_mixed_text() {
    let r = redact("a ghp_abcdefghijklmnopqrstuvwxyz0123456789 b ghp_abcdefghijklmnopqrstuvwxyz9876543210 c AKIAIOSFODNN7EXAMPLE");
    assert_eq!(r.count, 3);
    assert_eq!(r.kinds, ["github", "aws"]);
}

#[test]
fn empty_and_tiny_inputs() {
    assert!(redact("").is_clean());
    assert!(redact("ls").is_clean());
}

#[test]
fn unicode_text_is_safe() {
    let r = redact("contraseña=ñandú password=héroe123 «ok» ☃ token=abcd1234");
    assert!(r.text.contains('☃'));
    assert!(r.text.contains("«ok»"));
    assert!(!r.text.contains("abcd1234"));
}

#[test]
fn big_text_without_secrets_is_clean_and_fast() {
    let line = "INFO 2026-11-20 compiling crate foo v0.1.0 (C:\\src\\foo) in 1.2s\n";
    let text = line.repeat(20_000); // ~1.3 MB
    let t = std::time::Instant::now();
    let r = redact(&text);
    assert!(r.is_clean());
    assert!(
        t.elapsed() < std::time::Duration::from_secs(2),
        "took {:?}",
        t.elapsed()
    );
}
