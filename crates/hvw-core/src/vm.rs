//! Names of worktree VMs.

use sha2::{Digest, Sha256};

/// The longest VM name. Incus accepts 63 characters.
const MAX_LEN: usize = 63;

/// A name for a VM: it is also an Incus instance name, a systemd instance
/// name and a path component.
pub fn valid_vm_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_LEN
        && b[0].is_ascii_lowercase()
        && *b.last().unwrap_or(&b'-') != b'-'
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// The VM name of a worktree: `hvw-`, the last path component in a safe
/// form, and a hash of the full path. Two worktrees with the same last
/// component get different names. The same path always gets the same
/// name, so that a hook can find the VM of a worktree again.
pub fn vm_name(worktree: &str) -> String {
    let worktree = worktree.trim_end_matches('/');
    let hash = Sha256::digest(worktree.as_bytes());
    let suffix: String = hash[..4].iter().map(|b| format!("{b:02x}")).collect();
    let base = worktree.rsplit('/').next().unwrap_or_default();
    let mut slug = String::new();
    for c in base.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    // "hvw-" + slug + "-" + 8 hex digits.
    let room = MAX_LEN - 4 - 1 - suffix.len();
    let slug: String = slug.chars().take(room).collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("hvw-{suffix}")
    } else {
        format!("hvw-{slug}-{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(valid_vm_name("hvw-feat-x1"));
        assert!(!valid_vm_name(""));
        assert!(!valid_vm_name("1abc"));
        assert!(!valid_vm_name("abc-"));
        assert!(!valid_vm_name("a_b"));
        assert!(!valid_vm_name("Abc"));
        assert!(!valid_vm_name("a/b"));
        assert!(!valid_vm_name(&"a".repeat(64)));
    }

    #[test]
    fn worktree_names() {
        let a = vm_name("/home/u/.herdr/worktrees/app/Feat_Login");
        assert!(a.starts_with("hvw-feat-login-"), "{a}");
        assert!(valid_vm_name(&a), "{a}");
        assert_eq!(a, vm_name("/home/u/.herdr/worktrees/app/Feat_Login/"));
        assert_ne!(a, vm_name("/home/u/.herdr/worktrees/other/Feat_Login"));

        let long = vm_name(&format!("/w/{}", "x".repeat(200)));
        assert!(valid_vm_name(&long), "{long}");
        assert_eq!(long.len(), 63);

        for odd in ["/", "/w/---", "/w/ünïcode", "/w/-x-"] {
            let n = vm_name(odd);
            assert!(valid_vm_name(&n), "{odd} -> {n}");
        }
    }
}
