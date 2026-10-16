//! Masking of secrets in one-line labels and longer text.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// A word that says "secret" when it is part of a name.
const NAME: &str = "(?:key|token|secret|pass|auth|credential|private)";
/// A value: up to a space, a quote, a separator or a closing bracket, and one closing quote.
const VALUE: &str = r#"[^\s"',;}]+["']?"#;
/// Credential schemes an `Authorization` header may carry; these allow any whitespace, a
/// newline included, before the credential.
const SCHEMES: &str = "Bearer|Basic|Token|Digest|Negotiate|NTLM|OAuth|Bot|ApiKey|JWT|Splunk|Hawk|MAC|AWS4-HMAC-SHA256";
/// Strong token prefixes, masked wherever they start a word. `npm_`, `hf_` and `AKIA` and
/// `AIza` need a body, because the bare prefix is a common name part.
const PREFIXED: &str = r"\b(?:(?:sk_live_|sk_test_|sk-|ghp_|gho_|ghs_|ghu_|ghr_|github_pat_|glpat-|xox[abp]-|xapp-|crw_live_)[\w.\-]*|npm_[A-Za-z0-9]{16,}|hf_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{12,}|AIza[\w\-]{20,}|eyJ[\w\-]{5,}\.[\w\-]{5,}\.[\w\-]*)";

fn rx(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a fixed pattern")
}

struct Rules {
    pem: Regex,
    header_known: Regex,
    header: Regex,
    bearer: Regex,
    url_credentials: Regex,
    flag_value: Regex,
    user_password: Regex,
    db_password: Regex,
    key_value: Regex,
    json_pair: Regex,
    header_colon: Regex,
    prefixed: Regex,
}

static RULES: LazyLock<Rules> = LazyLock::new(|| {
    let header = r#"(\b(?:proxy-)?authorization\b["']?\s*[:=]\s*["']?)"#;
    Rules {
        pem: rx(
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY[A-Z0-9 ]*-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY[A-Z0-9 ]*-----|\z)",
        ),
        header_known: rx(&format!(r"(?i){header}({SCHEMES})(\s+)({VALUE})")),
        header: rx(&format!(
            r"(?i){header}(?:([A-Za-z][\w-]*)([ \t]+))?({VALUE})"
        )),
        bearer: rx(&format!(r"(?i)\b(bearer)([ \t]+)({VALUE})")),
        url_credentials: rx(r"(?i)([a-z][a-z0-9+.\-]*://[^\s/@:]*:)([^\s/@]+)(@)"),
        flag_value: rx(&format!(r"(?i)(\s--?[\w\-]*{NAME}[\w\-]*)([ \t]+)(\S+)")),
        user_password: rx(
            r#"(?i)(\s(?:-u|--user|--proxy-user)[ \t]*=?[ \t]*)([^\s:'"]+):([^\s'"]+)"#,
        ),
        db_password: rx(
            r"(\b(?:mysql\w*|mariadb)\b[^\n|;&]*?\s-p|\bsshpass\b[^\n|;&]*?\s-p[ \t]+)(\S+)",
        ),
        key_value: rx(&format!(
            r#"(?i)([\w.\-]*{NAME}[\w.\-]*)=("[^"]*"|'[^']*'|[^\s&]+)"#
        )),
        json_pair: rx(&format!(
            r#"(?i)("[\w.\-]*{NAME}[\w.\-]*"\s*:\s*)"(?:[^"\\]|\\.)*""#
        )),
        header_colon: rx(r#"(?i)\b([a-z]\w*(?:-\w+)+)(\s*:[ \t]*)([^\s"',}]+)"#),
        prefixed: rx(PREFIXED),
    }
});

/// Mask what looks like a secret. Covered: private key blocks, `Authorization` and
/// `Proxy-Authorization` values of any scheme, a word after `Bearer`, credentials in a URL,
/// the value of a `NAME=value`, a flag (`--password x`, `--token x`, `-u user:pass`,
/// `mysql -psecret`, `sshpass -p x`), a JSON string value whose key says key, token, secret,
/// pass, auth, credential or private, a header such as `X-API-Key: x`, and words that start like
/// a provider's token (`sk-`, `ghp_`, `glpat-`, `AKIA`, a three-part JWT, and more).
///
/// A label or a chat line travels further than the terminal that wrote it (the sidebar,
/// `qoop agents --json`, another machine's client), so this runs where text enters.
///
/// Best effort: a word list, not a scanner. A secret in a shape not listed here passes, and
/// an innocent word that looks like one is masked (`author=bob`). A bare `-p x` is left alone,
/// since it is `mkdir -p`; a `key: value` without quotes or a hyphenated name is left alone,
/// since it is code.
pub fn redact(text: &str) -> String {
    let r = &*RULES;
    let mut s = r.pem.replace_all(text, "***").into_owned();
    s = r
        .header_known
        .replace_all(&s, "${1}${2}${3}***")
        .into_owned();
    s = r.header.replace_all(&s, "${1}${2}${3}***").into_owned();
    s = r.bearer.replace_all(&s, "${1}${2}***").into_owned();
    s = r
        .url_credentials
        .replace_all(&s, "${1}***${3}")
        .into_owned();
    s = mask_flag_values(&r.flag_value, &s);
    s = r.user_password.replace_all(&s, "${1}${2}:***").into_owned();
    s = r.db_password.replace_all(&s, "${1}***").into_owned();
    s = r.key_value.replace_all(&s, "${1}=***").into_owned();
    s = r.json_pair.replace_all(&s, "${1}\"***\"").into_owned();
    s = mask_header_colon(&r.header_colon, &s);
    s = r.prefixed.replace_all(&s, "***").into_owned();
    s
}

/// `--password x`: the next word, unless it is another flag.
fn mask_flag_values(re: &Regex, s: &str) -> String {
    re.replace_all(s, |c: &Captures| {
        if c[3].starts_with('-') {
            c[0].to_string()
        } else {
            format!("{}{}***", &c[1], &c[2])
        }
    })
    .into_owned()
}

/// `X-API-Key: x`: a hyphenated name that says secret, other than the `Authorization` pair
/// that has its own rule.
fn mask_header_colon(re: &Regex, s: &str) -> String {
    re.replace_all(s, |c: &Captures| {
        let name = c[1].to_ascii_lowercase();
        let says_secret = [
            "key",
            "token",
            "secret",
            "pass",
            "auth",
            "credential",
            "private",
        ]
        .iter()
        .any(|n| name.contains(n));
        if says_secret && !name.contains("authorization") {
            format!("{}{}***", &c[1], &c[2])
        } else {
            c[0].to_string()
        }
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::redact;

    const SECRET: &str = "abc123secret";

    /// Each shape: the text, and the part of it that must not survive.
    fn shapes() -> Vec<(String, String)> {
        let s = SECRET;
        let long = |prefix: &str| format!("{prefix}{s}XYZ0123456789abcdef");
        let mut table: Vec<(String, String)> = [
            // JSON
            (format!(r#"{{"token":"{s}"}}"#), s.to_string()),
            (format!(r#"{{"api_key": "{s}", "n": 1}}"#), s.to_string()),
            (format!(r#"{{"password":"{s}","user":"bob"}}"#), s.to_string()),
            // URLs
            (format!("curl 'https://x.io/y?a=1&api_key={s}&b=2'"), s.to_string()),
            (format!("https://x.io/cb?access_token={s}"), s.to_string()),
            (format!("git clone https://bob:{s}@github.com/x/y.git"), s.to_string()),
            (format!("postgres://app:{s}@db:5432/main"), s.to_string()),
            // KEY=value
            (format!("export MY_SECRET_VALUE={s}"), s.to_string()),
            (format!("DB_PASSWORD=\"{s} with spaces\" ./run"), s.to_string()),
            (format!("AWS_CREDENTIAL={s} aws s3 ls"), s.to_string()),
            (format!("PRIVATE_NOTE={s}"), s.to_string()),
            (format!("x=sk_live_{s}0000"), format!("sk_live_{s}")),
            // Authorization, any scheme, any whitespace
            (format!("Authorization: Bearer {s}"), s.to_string()),
            (format!("authorization: Basic {s}"), s.to_string()),
            (format!("Authorization: Token {s}"), s.to_string()),
            (format!("Proxy-Authorization: Basic {s}"), s.to_string()),
            (format!("Authorization:\tBearer\t{s}"), s.to_string()),
            (format!("Authorization: Bearer\n{s}"), s.to_string()),
            (format!("Authorization: Bearer\r\n   {s}"), s.to_string()),
            (format!("Authorization: {s}"), s.to_string()),
            (format!(r#"{{"Authorization": "Bearer {s}"}}"#), s.to_string()),
            (format!("-H 'Authorization: Digest {s}'"), s.to_string()),
            (format!("fetch with Bearer {s} now"), s.to_string()),
            // headers
            (format!("X-API-Key: {s}"), s.to_string()),
            (format!("-H 'x-auth-token: {s}'"), s.to_string()),
            (format!(r#"{{"X-Api-Key": "{s}"}}"#), s.to_string()),
            // flags
            (format!("tool --api-key {s} run"), s.to_string()),
            (format!("tool --token {s}"), s.to_string()),
            (format!("tool --password {s}"), s.to_string()),
            (format!("tool --password={s}"), s.to_string()),
            (format!("curl -u admin:{s} https://x.io"), s.to_string()),
            (format!("curl -uadmin:{s} https://x.io"), s.to_string()),
            (format!("curl --user admin:{s} https://x.io"), s.to_string()),
            (format!("mysql -u root -p{s} main"), s.to_string()),
            (format!("sshpass -p {s} ssh host"), s.to_string()),
            // PEM blocks
            (
                format!("before\n-----BEGIN RSA PRIVATE KEY-----\nMIIE{s}\nline2{s}\n-----END RSA PRIVATE KEY-----\nafter"),
                s.to_string(),
            ),
            (format!("-----BEGIN PRIVATE KEY-----\nMIIE{s}\n(cut off)"), s.to_string()),
            (
                format!("-----BEGIN OPENSSH PRIVATE KEY-----\nb3Bl{s}\n-----END OPENSSH PRIVATE KEY-----"),
                s.to_string(),
            ),
            // JWT
            (format!("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.{s}"), s.to_string()),
        ]
        .into_iter()
        .collect();
        for prefix in [
            "sk_live_",
            "sk-",
            "sk-ant-api03-",
            "glpat-",
            "ghp_",
            "ghs_",
            "gho_",
            "xoxb-",
            "xoxa-",
            "xoxp-",
            "github_pat_",
        ] {
            table.push((format!("use {} now", long(prefix)), long(prefix)));
        }
        for token in [
            format!("npm_{s}Z0123456789"),
            format!("hf_{s}Z0123456789abcdef"),
            "AKIAABCDEFGHIJKLMNOP".to_string(),
            format!("AIzaSy{s}Z0123456789abcdef"),
        ] {
            table.push((format!("tool {token} end"), token));
        }
        table
    }

    #[test]
    fn every_secret_shape_is_masked() {
        for (text, secret) in shapes() {
            let out = redact(&text);
            assert!(!out.contains(&secret), "{text:?} -> {out:?}");
            assert!(out.contains("***"), "{text:?} -> {out:?}");
            assert_eq!(redact(&out), out, "masking twice changes {text:?}");
        }
    }

    #[test]
    fn what_surrounds_a_secret_is_kept() {
        let s = SECRET;
        let cases = [
            (
                format!("curl 'https://x.io/y?a=1&api_key={s}&b=2'"),
                "a=1&api_key=***&b=2",
            ),
            (
                format!("git clone https://bob:{s}@github.com/x"),
                "https://bob:***@github.com/x",
            ),
            (
                format!(r#"{{"password":"{s}","user":"bob"}}"#),
                r#""password":"***","user":"bob""#,
            ),
            (
                format!("Authorization:\tBearer\t{s}"),
                "Authorization:\tBearer\t***",
            ),
            (
                format!("tool --token {s} --verbose"),
                "tool --token *** --verbose",
            ),
            (
                format!("curl -u admin:{s} https://x.io"),
                "-u admin:*** https://x.io",
            ),
            (
                format!("a\n-----BEGIN PRIVATE KEY-----\n{s}\n-----END PRIVATE KEY-----\nz"),
                "a\n***\nz",
            ),
        ];
        for (text, kept) in cases {
            let out = redact(&text);
            assert!(out.contains(kept), "{out:?} lacks {kept:?}");
        }
    }

    #[test]
    fn ordinary_text_and_commands_are_not_masked() {
        for text in [
            "mkdir -p /tmp/x && ls -p",
            "gcc -pthread -o a a.c",
            r#"{"passed": true, "max_tokens": 4096, "keys": [1, 2]}"#,
            "if a < b > c then d",
            "pub token: String,",
            "npm_config_cache=/tmp/npm",
            "the task-force halfway home",
            "tool --keep-going run",
            "docker run -p 8080:80 -u 1000 img",
            "Authorization is explained in the docs",
        ] {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn a_one_line_label_keeps_its_old_masking() {
        assert_eq!(
            redact("Bash curl -H \"Authorization: Bearer ghx123\" https://x"),
            "Bash curl -H \"Authorization: Bearer *** https://x"
        );
        assert_eq!(redact("sk-ant-api03-abcdef rest"), "*** rest");
        assert_eq!(
            redact("OPENAI_API_KEY=sk-abc npm test"),
            "OPENAI_API_KEY=*** npm test"
        );
        assert_eq!(redact("one\nBearer abc\nghp_xyz"), "one\nBearer ***\n***");
    }
}
