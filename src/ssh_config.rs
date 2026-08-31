//! Bounded OpenSSH config discovery. Reading config never executes commands.
//! Supported connection settings use OpenSSH's first-value-wins ordering.

use std::{collections, fs, io, path};

use anyhow::Context;

const MAX_BYTES: u64 = 1024 * 1024;
const MAX_FILES: usize = 128;
const MAX_DEPTH: usize = 12;

#[derive(Clone, Debug)]
struct Line {
    keyword: String,
    values: Vec<String>,
    included: Vec<Line>,
}

#[derive(Default)]
pub struct Config {
    lines: Vec<Line>,
    aliases: Vec<String>,
    home: path::PathBuf,
}

#[derive(Default, Debug)]
pub struct Profile {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<path::PathBuf>,
    pub known_hosts: Option<path::PathBuf>,
    pub identities_only: bool,
    /// Retain these in the form and refuse to silently bypass them on Connect.
    pub unsupported: Vec<String>,
}

impl Config {
    /// A missing config is normal. Existing but unreadable/malformed files are
    /// reported by the UI; the application does not invent a replacement.
    pub fn load(home: &path::Path) -> anyhow::Result<Self> {
        let mut reader = Reader {
            base: home.join(".ssh"),
            home,
            bytes: 0,
            files: 0,
            stack: collections::BTreeSet::new(),
        };
        let file = reader.base.join("config");
        let lines = match fs::metadata(&file) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            _ => reader.read(&file, 0)?,
        };
        let mut aliases = collections::BTreeSet::new();
        collect_aliases(&lines, &mut aliases);
        Ok(Self {
            lines,
            aliases: aliases.into_iter().collect(),
            home: home.to_owned(),
        })
    }

    #[cfg(test)]
    pub(crate) fn from_text(text: &str) -> Self {
        let lines: Vec<_> = text
            .lines()
            .filter_map(|line| {
                let mut values = words(line).unwrap();
                if values.is_empty() {
                    return None;
                }
                Some(Line {
                    keyword: values.remove(0).to_ascii_lowercase(),
                    values,
                    included: Vec::new(),
                })
            })
            .collect();
        let mut aliases = collections::BTreeSet::new();
        collect_aliases(&lines, &mut aliases);
        Self {
            lines,
            aliases: aliases.into_iter().collect(),
            home: "/home/test".into(),
        }
    }

    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    /// The first existing OpenSSH default identity Starcom can sign.
    ///
    /// OpenSSH offers every default file plus agent keys. Starcom's form holds
    /// one file, so this prefers Ed25519 over leftover RSA/ECDSA files.
    pub fn default_identity(&self) -> Option<path::PathBuf> {
        first_default_identity(&self.home)
    }

    pub fn resolve(&self, alias: &str) -> anyhow::Result<Profile> {
        anyhow::ensure!(
            !alias.is_empty() && alias.len() <= 255 && !alias.chars().any(char::is_control),
            "invalid SSH host alias"
        );
        let mut values = collections::BTreeMap::<String, Vec<String>>::new();
        let mut match_seen = false;
        evaluate(&self.lines, alias, &mut values, &mut match_seen);
        let first = |key: &str| values.get(key).and_then(|values| values.first());
        let host = first("hostname")
            .map(|host| host.replace("%h", alias).replace("%n", alias))
            .unwrap_or_else(|| alias.to_owned());
        anyhow::ensure!(!host.contains('%'), "unsupported token in HostName");
        let mut profile = Profile {
            host,
            user: first("user").cloned(),
            port: first("port").map(|port| port.parse()).transpose()?,
            identities_only: first("identitiesonly")
                .is_some_and(|value| value.eq_ignore_ascii_case("yes")),
            ..Profile::default()
        };
        if let Some(value) = first("identityfile") {
            if values["identityfile"].len() > 1 {
                profile
                    .unsupported
                    .push("multiple IdentityFile entries".into());
            } else if value != "none" {
                profile.identity = Some(expand_path(value, &self.home, alias, &profile)?);
            } else {
                profile.unsupported.push("IdentityFile none".into());
            }
        }
        if let Some(value) = first("userknownhostsfile") {
            if values["userknownhostsfile"].len() != 1 || value == "none" {
                profile
                    .unsupported
                    .push("multiple/disabled UserKnownHostsFile".into());
            } else {
                profile.known_hosts = Some(expand_path(value, &self.home, alias, &profile)?);
            }
        }
        // These options change routing, authentication, or trust. Display them
        // as blockers rather than connecting directly or using another identity.
        for (name, neutral) in [
            ("proxyjump", "none"),
            ("proxycommand", "none"),
            ("certificatefile", "none"),
            ("identityagent", "SSH_AUTH_SOCK"),
            ("hostkeyalias", ""),
            ("globalknownhostsfile", "none"),
            ("canonicalizehostname", "no"),
            ("bindaddress", ""),
            ("bindinterface", ""),
            ("hostkeyalgorithms", ""),
            ("pubkeyacceptedalgorithms", ""),
            ("kexalgorithms", ""),
            ("ciphers", ""),
            ("macs", ""),
            ("revokedhostkeys", "none"),
            ("requiredrsasize", ""),
        ] {
            if let Some(value) = first(name)
                && value != neutral
            {
                profile.unsupported.push(name.to_owned());
            }
        }
        if first("identitiesonly").is_some_and(|value| value.eq_ignore_ascii_case("yes"))
            && profile.identity.is_none()
            && self.default_identity().is_none()
        {
            profile
                .unsupported
                .push("IdentitiesOnly without an explicit IdentityFile".into());
        }
        if first("pubkeyauthentication").is_some_and(|value| value.eq_ignore_ascii_case("no")) {
            profile.unsupported.push("PubkeyAuthentication no".into());
        }
        if let Some(value) = first("preferredauthentications")
            && !value.split(',').any(|method| method == "publickey")
        {
            profile
                .unsupported
                .push("PreferredAuthentications excludes publickey".into());
        }
        if match_seen {
            profile
                .unsupported
                .push("Match conditions (not evaluated or executed)".into());
        }
        Ok(profile)
    }
}

fn evaluate(
    lines: &[Line],
    alias: &str,
    values: &mut collections::BTreeMap<String, Vec<String>>,
    match_seen: &mut bool,
) {
    let mut active = true;
    for line in lines {
        match line.keyword.as_str() {
            "host" => active = matches_host(&line.values, alias),
            "match" => {
                // Conditions can depend on remote routing, local commands, or
                // a second canonicalization pass. Never approximate their policy.
                *match_seen = true;
                active = false;
            }
            "include" if active => evaluate(&line.included, alias, values, match_seen),
            "include" => {}
            "identityfile" if active => {
                values
                    .entry(line.keyword.clone())
                    .or_default()
                    .extend(line.values.clone());
            }
            _ if active => {
                values
                    .entry(line.keyword.clone())
                    .or_insert_with(|| line.values.clone());
            }
            _ => {}
        }
    }
}

fn collect_aliases(lines: &[Line], names: &mut collections::BTreeSet<String>) {
    for line in lines {
        if line.keyword == "host" {
            for name in &line.values {
                if !name.is_empty() && !name.contains(['*', '?', '!']) {
                    names.insert(name.clone());
                }
            }
        }
        collect_aliases(&line.included, names);
    }
}

fn matches_host(patterns: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let mut matched = false;
    for pattern in patterns {
        let (negative, pattern) = pattern
            .strip_prefix('!')
            .map_or((false, pattern.as_str()), |p| (true, p));
        if glob_matches(&pattern.to_ascii_lowercase(), &host) {
            if negative {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// Linear-space wildcard matching; no regular-expression engine or recursion.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < t.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(at) = star {
            retry += 1;
            j = retry;
            i = at + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

fn words(line: &str) -> anyhow::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escape = false;
    for ch in line.chars() {
        if escape {
            // Preserve Windows paths while handling escaped whitespace/quotes.
            if !matches!(ch, ' ' | '\t' | '"' | '\'' | '\\' | '#') {
                word.push('\\');
            }
            word.push(ch);
            escape = false;
        } else if ch == '\\' {
            escape = true;
        } else if Some(ch) == quote {
            quote = None;
        } else if quote.is_some() {
            word.push(ch);
        } else if matches!(ch, '"' | '\'') {
            quote = Some(ch);
        } else if ch == '#' {
            break;
        } else if ch.is_whitespace()
            || (ch == '=' && (words.is_empty() || (words.len() == 1 && word.is_empty())))
        {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if escape {
        word.push('\\');
    }
    anyhow::ensure!(quote.is_none(), "unterminated quote in SSH config");
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

/// Names OpenSSH would try with no IdentityFile, among algorithms Starcom signs.
/// Hardware-backed `*_sk`, DSA, and XMSS keys are omitted: they are unsupported.
const DEFAULT_IDENTITY_FILES: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];

fn first_default_identity(home: &path::Path) -> Option<path::PathBuf> {
    DEFAULT_IDENTITY_FILES.iter().find_map(|name| {
        let path = home.join(".ssh").join(name);
        path.is_file().then_some(path)
    })
}

fn expand_path(
    value: &str,
    home: &path::Path,
    alias: &str,
    profile: &Profile,
) -> anyhow::Result<path::PathBuf> {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('d') => out.push_str(home.to_str().context("home path is not UTF-8")?),
            Some('h') => out.push_str(&profile.host),
            Some('n') => out.push_str(alias),
            Some('r') => out.push_str(
                profile
                    .user
                    .as_deref()
                    .context("%r needs an explicit User")?,
            ),
            Some('p') => out.push_str(&profile.port.unwrap_or(22).to_string()),
            _ => anyhow::bail!("unsupported SSH path token"),
        }
    }
    anyhow::ensure!(
        !out.contains("${"),
        "environment expansion in SSH paths is not supported yet"
    );
    if let Some(tail) = out.strip_prefix("~/").or_else(|| out.strip_prefix("~\\")) {
        Ok(home.join(tail))
    } else {
        anyhow::ensure!(
            !out.starts_with('~'),
            "other-user home expansion is not supported"
        );
        let path = path::PathBuf::from(out);
        // OpenSSH resolves a relative IdentityFile against the process directory.
        Ok(path)
    }
}

struct Reader<'a> {
    base: path::PathBuf,
    home: &'a path::Path,
    bytes: u64,
    files: usize,
    stack: collections::BTreeSet<path::PathBuf>,
}

impl Reader<'_> {
    fn read(&mut self, file: &path::Path, depth: usize) -> anyhow::Result<Vec<Line>> {
        anyhow::ensure!(
            depth < MAX_DEPTH && self.files < MAX_FILES,
            "SSH Include limit exceeded"
        );
        let canonical = file
            .canonicalize()
            .with_context(|| format!("read SSH config {}", file.display()))?;
        anyhow::ensure!(
            self.stack.insert(canonical.clone()),
            "cyclic SSH Include at {}",
            file.display()
        );
        self.files += 1;
        let mut text = String::new();
        io::Read::read_to_string(
            &mut io::Read::take(
                fs::File::open(file)?,
                MAX_BYTES.saturating_sub(self.bytes) + 1,
            ),
            &mut text,
        )?;
        self.bytes += text.len() as u64;
        anyhow::ensure!(self.bytes <= MAX_BYTES, "SSH config exceeds 1 MiB total");
        let mut lines = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let mut parts =
                words(line).with_context(|| format!("{}:{}", file.display(), index + 1))?;
            if parts.is_empty() {
                continue;
            }
            let keyword = parts.remove(0).to_ascii_lowercase();
            anyhow::ensure!(
                !parts.is_empty(),
                "missing SSH config value at {}:{}",
                file.display(),
                index + 1
            );
            if matches!(
                keyword.as_str(),
                "hostname"
                    | "user"
                    | "port"
                    | "identityfile"
                    | "identitiesonly"
                    | "identityagent"
                    | "hostkeyalias"
            ) {
                anyhow::ensure!(
                    parts.len() == 1,
                    "extra values for {keyword} at {}:{}",
                    file.display(),
                    index + 1
                );
            }
            let mut included = Vec::new();
            if keyword == "include" {
                for pattern in &parts {
                    let path = if let Some(tail) = pattern.strip_prefix("~/") {
                        self.home.join(tail)
                    } else if path::Path::new(pattern).is_absolute() {
                        pattern.into()
                    } else {
                        self.base.join(pattern)
                    };
                    for file in expand_glob(&path)? {
                        // OpenSSH restores the containing Host/Match state
                        // after EACH included file, not only the whole glob.
                        included.push(Line {
                            keyword: "include".into(),
                            values: Vec::new(),
                            included: self.read(&file, depth + 1)?,
                        });
                    }
                }
            }
            lines.push(Line {
                keyword,
                values: parts,
                included,
            });
        }
        self.stack.remove(&canonical);
        Ok(lines)
    }
}

fn expand_glob(pattern: &path::Path) -> anyhow::Result<Vec<path::PathBuf>> {
    let mut paths = vec![path::PathBuf::new()];
    for component in pattern.components() {
        let text = component.as_os_str().to_string_lossy();
        anyhow::ensure!(
            !text.contains(['[', ']', '{', '}']),
            "unsupported Include glob: {}",
            pattern.display()
        );
        if text.contains(['*', '?']) {
            let mut next = Vec::new();
            for base in &paths {
                let entries = match fs::read_dir(base) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                for (count, entry) in entries.enumerate() {
                    anyhow::ensure!(
                        count < 4096,
                        "SSH Include directory exceeds discovery budget"
                    );
                    let entry = entry?;
                    if glob_matches(&text, &entry.file_name().to_string_lossy()) {
                        next.push(entry.path());
                        anyhow::ensure!(next.len() <= MAX_FILES, "too many SSH Include matches");
                    }
                }
            }
            next.sort();
            paths = next;
        } else {
            for path in &mut paths {
                path.push(component);
            }
        }
    }
    // An unmatched Include is explicitly allowed by OpenSSH.
    let mut existing = Vec::new();
    for path in paths {
        match fs::metadata(&path) {
            Ok(_) => existing.push(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(existing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        let lines = text
            .lines()
            .filter_map(|line| {
                let mut values = words(line).unwrap();
                if values.is_empty() {
                    return None;
                }
                Some(Line {
                    keyword: values.remove(0).to_ascii_lowercase(),
                    values,
                    included: Vec::new(),
                })
            })
            .collect();
        Config {
            lines,
            home: "/home/test".into(),
            ..Config::default()
        }
    }

    #[test]
    fn first_value_wins_and_host_patterns_exclude_negations() {
        let config = config(
            "Host dev backup\n HostName 10.2.3.4\n User specific\n Port=2222\nHost * !backup\n User fallback\nHost *\n Port 22",
        );
        let dev = config.resolve("dev").unwrap();
        assert_eq!(dev.host, "10.2.3.4");
        assert_eq!(dev.user.as_deref(), Some("specific"));
        assert_eq!(dev.port, Some(2222));
        assert_eq!(
            config.resolve("other").unwrap().user.as_deref(),
            Some("fallback")
        );
        assert!(!matches_host(&["*".into(), "!backup".into()], "backup"));
    }

    #[test]
    fn quoted_paths_and_percent_tokens_are_resolved() {
        let config = config(
            "Host dev\nHostName dev.test\nUser alice\nIdentityFile \"~/.ssh/my # key\" # comment\nUserKnownHostsFile %d/.ssh/known_%h",
        );
        let profile = config.resolve("dev").unwrap();
        assert_eq!(
            profile.identity.unwrap(),
            path::PathBuf::from("/home/test/.ssh/my # key")
        );
        assert_eq!(
            profile.known_hosts.unwrap(),
            path::PathBuf::from("/home/test/.ssh/known_dev.test")
        );
        assert!(words("Host \"missing").is_err());
    }

    #[test]
    fn unsupported_routing_and_match_are_never_silently_bypassed() {
        let config = config("Host remote\nProxyJump gateway\nHost local\nHostName 127.0.0.1");
        assert_eq!(config.resolve("remote").unwrap().unsupported, ["proxyjump"]);
        assert!(config.resolve("local").unwrap().unsupported.is_empty());
        let config = self::config(
            "Match exec \"touch /must-not-run\"\nUser altered\nHost safe\nHostName localhost",
        );
        assert!(!config.resolve("safe").unwrap().unsupported.is_empty());
    }

    #[test]
    fn aliases_are_literal_unique_and_sorted() {
        let config = config("Host *.example !bad z a\nHost a\nHost b?");
        let mut names = collections::BTreeSet::new();
        collect_aliases(&config.lines, &mut names);
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["a", "z"]);
    }

    #[test]
    fn included_files_are_sorted_bounded_and_cycles_fail() {
        let root = std::env::temp_dir().join(format!(
            "starcom-config-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Cleanup(path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        fs::create_dir_all(root.join(".ssh/conf.d")).unwrap();
        fs::write(
            root.join(".ssh/config"),
            "Include conf.d/*\nHost *\nUser fallback\n",
        )
        .unwrap();
        fs::write(
            root.join(".ssh/conf.d/a"),
            "Host alpha\nHostName 10.0.0.1\nUser first\n",
        )
        .unwrap();
        fs::write(root.join(".ssh/conf.d/b"), "Host alpha beta\nUser second\n").unwrap();
        let config = Config::load(&root).unwrap();
        assert_eq!(config.aliases(), ["alpha", "beta"]);
        assert_eq!(
            config.resolve("alpha").unwrap().user.as_deref(),
            Some("first")
        );
        assert_eq!(
            config.resolve("beta").unwrap().user.as_deref(),
            Some("second")
        );
        // An included file ending in a non-matching Host must not disable
        // defaults at the beginning of the next file in the glob.
        fs::write(
            root.join(".ssh/conf.d/b"),
            "Port 2222\nHost beta\nUser second\n",
        )
        .unwrap();
        assert_eq!(
            Config::load(&root).unwrap().resolve("other").unwrap().port,
            Some(2222)
        );
        fs::write(root.join(".ssh/conf.d/a"), "Include config\n").unwrap();
        assert!(Config::load(&root).is_err());
    }

    #[test]
    fn omitted_identityfile_selects_ed25519_over_a_leftover_rsa_file() {
        let root = std::env::temp_dir().join(format!(
            "starcom-identity-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Cleanup(path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        fs::create_dir_all(root.join(".ssh")).unwrap();
        fs::write(
            root.join(".ssh/config"),
            "Host zork\nHostName zork.example\n",
        )
        .unwrap();
        fs::write(root.join(".ssh/id_rsa"), "rsa").unwrap();
        fs::write(root.join(".ssh/id_ed25519"), "ed25519").unwrap();
        let config = Config::load(&root).unwrap();
        assert_eq!(
            config.default_identity(),
            Some(root.join(".ssh/id_ed25519"))
        );
        let profile = config.resolve("zork").unwrap();
        assert!(profile.identity.is_none());
        assert!(profile.unsupported.is_empty());
        fs::write(root.join(".ssh/config"), "Host zork\nIdentitiesOnly yes\n").unwrap();
        let config = Config::load(&root).unwrap();
        assert!(config.resolve("zork").unwrap().unsupported.is_empty());
        fs::remove_file(root.join(".ssh/id_ed25519")).unwrap();
        fs::remove_file(root.join(".ssh/id_rsa")).unwrap();
        assert!(config.default_identity().is_none());
        assert_eq!(
            Config::load(&root)
                .unwrap()
                .resolve("zork")
                .unwrap()
                .unsupported,
            ["IdentitiesOnly without an explicit IdentityFile"]
        );
    }
}
