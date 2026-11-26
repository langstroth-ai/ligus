//! Secrets given to the CLI as flags (`--secret`, `--reg-token`, `--token`)
//! and secret files (`--recovery-words-file`).
//!
//! A value on the command line leaks to `ps`, `/proc/<pid>/cmdline` and shell
//! history, so every secret flag also takes `-` (read it from stdin) or
//! `@FILE` (read it from a file), and warns when it is given literally, as
//! `ligus enroll` does for the invite. A secret file must be a regular file
//! that only its owner can read (the `lt` terminal app's rule): never a FIFO
//! or a device, which could block or stream forever, and never one other
//! users could have read already.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// The most a secret flag's value may be when read from stdin or a file.
pub const MAX_SECRET_BYTES: u64 = 64 * 1024;

/// Read a secret from a file: a regular file (never a FIFO or a device,
/// which could block or stream forever), owner-only on Unix, and at most
/// `max` bytes (never read into memory whole when it is bigger). A link is
/// followed (a mounted Kubernetes Secret is one); the file opened is the one
/// checked.
pub fn read_owner_only(path: &Path, max: u64) -> Result<String> {
    use std::io::Read;
    let shown = path.display();
    // Checked before opening: opening a FIFO for reading blocks.
    let before = std::fs::metadata(path).with_context(|| format!("reading {shown}"))?;
    if !before.is_file() {
        bail!("{shown} isn't a regular file");
    }
    let f = std::fs::File::open(path).with_context(|| format!("reading {shown}"))?;
    let meta = f.metadata().with_context(|| format!("reading {shown}"))?;
    if !meta.is_file() {
        bail!("{shown} isn't a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("others can read {shown}: a secret file must be owner-only (chmod 600 it)");
        }
    }
    if meta.len() > max {
        bail!("{shown} is too big for a secret");
    }
    let mut text = String::new();
    f.take(max + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("reading {shown}"))?;
    if text.len() as u64 > max {
        zeroize::Zeroize::zeroize(&mut text);
        bail!("{shown} is too big for a secret");
    }
    Ok(text)
}

/// Where a secret flag's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Stdin,
    File,
    /// Literally on the command line: warn.
    Argv,
}

/// The value of the secret flag `flag`: `-` reads stdin (once per command:
/// `stdin` errors on a second read), `@FILE` reads an owner-only file, and
/// anything else is the value itself, given on the command line. Trailing
/// line ends and surrounding whitespace are dropped.
pub fn secret_flag(
    flag: &str,
    value: Option<String>,
    stdin: &mut dyn FnMut() -> std::io::Result<String>,
) -> Result<Option<(String, Source)>> {
    let Some(v) = value else { return Ok(None) };
    let (text, source) = if v == "-" {
        let mut s = stdin().with_context(|| format!("reading {flag} from stdin"))?;
        let t = s.trim().to_string();
        zeroize::Zeroize::zeroize(&mut s);
        (t, Source::Stdin)
    } else if let Some(path) = v.strip_prefix('@') {
        let mut s = read_owner_only(Path::new(path), MAX_SECRET_BYTES)
            .with_context(|| format!("{flag} @{path}"))?;
        let t = s.trim().to_string();
        zeroize::Zeroize::zeroize(&mut s);
        (t, Source::File)
    } else {
        (v.trim().to_string(), Source::Argv)
    };
    if text.is_empty() {
        bail!("{flag} is empty");
    }
    Ok(Some((text, source)))
}

/// The warning for a secret given literally on the command line.
pub fn argv_warning(flag: &str) -> String {
    format!(
        "warning: {flag} is a secret, and the command line leaks it (ps, shell history): \
         next time pass {flag} - (read from stdin) or {flag} @FILE (an owner-only file)"
    )
}

/// [`secret_flag`], printing [`argv_warning`] on stderr when the value was
/// on the command line.
pub fn secret_flag_warn(
    flag: &str,
    value: Option<String>,
    stdin: &mut dyn FnMut() -> std::io::Result<String>,
) -> Result<Option<String>> {
    Ok(secret_flag(flag, value, stdin)?.map(|(v, source)| {
        if source == Source::Argv {
            eprintln!("{}", argv_warning(flag));
        }
        v
    }))
}

/// A stdin reader that reads once: a second secret asking for `-` in the
/// same command is an error, not an empty value.
pub fn stdin_once() -> impl FnMut() -> std::io::Result<String> {
    let mut used = false;
    move || {
        if used {
            return Err(std::io::Error::other(
                "stdin was already read for another value: use @FILE for this one",
            ));
        }
        used = true;
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        Ok(s)
    }
}

/// A recovery key from a recovery-words file: a regular, owner-only file of
/// at most 4 KiB. The words are wiped from memory once parsed.
pub fn recovery_key_from_file(path: &Path) -> Result<ligus_core::presence::RecoveryKey> {
    let mut words = read_owner_only(path, 4 * 1024).context("the recovery-words file")?;
    let key = ligus_core::presence::RecoveryKey::from_words(&words);
    zeroize::Zeroize::zeroize(&mut words);
    key.map_err(|e| anyhow::anyhow!(e.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_stdin() -> std::io::Result<String> {
        panic!("stdin not read")
    }

    /// A secret flag reads `-` from stdin and `@FILE` from an owner-only
    /// file, and says when the value was on the command line.
    #[test]
    fn secret_flags_take_stdin_or_a_file() {
        assert_eq!(secret_flag("--token", None, &mut no_stdin).unwrap(), None);
        assert_eq!(
            secret_flag("--token", Some("-".into()), &mut || Ok("hrt1abc\n".into())).unwrap(),
            Some(("hrt1abc".to_string(), Source::Stdin))
        );
        assert_eq!(
            secret_flag("--token", Some("hrt1abc".into()), &mut no_stdin).unwrap(),
            Some(("hrt1abc".to_string(), Source::Argv))
        );
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("tok");
        ligus_runtime::private_file::write_private_atomic(&f, b"sk-123\n").unwrap();
        assert_eq!(
            secret_flag("--secret", Some(format!("@{}", f.display())), &mut no_stdin).unwrap(),
            Some(("sk-123".to_string(), Source::File))
        );
        assert!(secret_flag("--secret", Some("@/nonexistent/x".into()), &mut no_stdin).is_err());
        assert!(
            secret_flag("--secret", Some("-".into()), &mut || Ok("  \n".into())).is_err(),
            "empty"
        );
        assert!(argv_warning("--reg-token").contains("--reg-token -"));
    }

    /// Two values asking for stdin in one command: the second is refused.
    #[test]
    fn stdin_is_read_once() {
        let mut used = false;
        let mut once = move || -> std::io::Result<String> {
            if used {
                return Err(std::io::Error::other("again"));
            }
            used = true;
            Ok("x".into())
        };
        assert!(secret_flag("--a", Some("-".into()), &mut once).is_ok());
        assert!(secret_flag("--b", Some("-".into()), &mut once).is_err());
    }

    fn refusal(r: Result<ligus_core::presence::RecoveryKey>) -> String {
        match r {
            Ok(_) => panic!("accepted"),
            Err(e) => format!("{e:#}"),
        }
    }

    /// A recovery-words file (and an `@FILE` secret) must be a regular file
    /// only its owner can read; anything else is refused before it is read.
    #[cfg(unix)]
    #[test]
    fn recovery_words_file_must_be_regular_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let key = ligus_core::presence::RecoveryKey::generate().unwrap();
        let good = dir.path().join("words");
        ligus_runtime::private_file::write_private_atomic(&good, key.words().as_bytes()).unwrap();
        assert_eq!(
            recovery_key_from_file(&good).unwrap().public_key(),
            key.public_key()
        );

        let open = dir.path().join("open");
        std::fs::write(&open, key.words()).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o644)).unwrap();
        let e = refusal(recovery_key_from_file(&open));
        assert!(e.contains("chmod 600"), "{e}");
        let e = format!(
            "{:#}",
            secret_flag(
                "--secret",
                Some(format!("@{}", open.display())),
                &mut no_stdin
            )
            .unwrap_err()
        );
        assert!(e.contains("chmod 600"), "{e}");

        // A directory, and a FIFO (which would block a plain read).
        assert!(refusal(recovery_key_from_file(dir.path())).contains("regular file"));
        let fifo = dir.path().join("fifo");
        if std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success())
        {
            assert!(refusal(recovery_key_from_file(&fifo)).contains("regular file"));
        }

        // Too big for a recovery key.
        let big = dir.path().join("big");
        ligus_runtime::private_file::write_private_atomic(&big, &vec![b'a'; 8 * 1024]).unwrap();
        assert!(refusal(recovery_key_from_file(&big)).contains("too big"));
    }

    /// The recovery key's seed is wiped when the key is dropped.
    #[test]
    fn recovery_key_zeroizes_its_seed() {
        fn wiped_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        wiped_on_drop::<ligus_core::presence::RecoveryKey>();
    }
}
