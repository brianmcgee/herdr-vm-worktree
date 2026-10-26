//! Farm planning. A farm is a btrfs subvolume that holds the store closure
//! of one devshell and the flake inputs that evaluate it. Equal closures
//! share one farm.

use std::collections::BTreeSet;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The name of a farm: a hash of its sorted closure.
pub fn key<'a>(closure: impl IntoIterator<Item = &'a str>) -> String {
    let sorted: BTreeSet<&str> = closure.into_iter().collect();
    let mut h = Sha256::new();
    for p in sorted {
        h.update(p.as_bytes());
        h.update(b"\n");
    }
    let digest = h.finalize();
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Deserialize)]
struct Archive {
    path: String,
    #[serde(default)]
    inputs: std::collections::BTreeMap<String, Archive>,
}

/// The store paths of a flake and of all its inputs, from the output of
/// `nix flake archive --json --dry-run`.
pub fn archive_paths(json: &str) -> Result<Vec<String>, serde_json::Error> {
    fn walk(a: &Archive, out: &mut BTreeSet<String>) {
        out.insert(a.path.clone());
        for i in a.inputs.values() {
            walk(i, out);
        }
    }
    let a: Archive = serde_json::from_str(json)?;
    let mut out = BTreeSet::new();
    walk(&a, &mut out);
    Ok(out.into_iter().collect())
}

/// A farm on disk, for garbage collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FarmUse {
    pub key: String,
    /// Seconds since the Unix epoch of the last launch that used it.
    pub last_used: u64,
}

/// The farms to delete: no VM uses them and the last use is older than
/// `retention` seconds.
pub fn expired<'a>(
    farms: &'a [FarmUse],
    in_use: &BTreeSet<String>,
    now: u64,
    retention: u64,
) -> Vec<&'a str> {
    farms
        .iter()
        .filter(|f| !in_use.contains(&f.key) && now.saturating_sub(f.last_used) > retention)
        .map(|f| f.key.as_str())
        .collect()
}

/// True if `p` is a direct child of `/nix/store`, with no `..`.
pub fn is_store_path(p: &str) -> bool {
    p.strip_prefix("/nix/store/").is_some_and(|name| {
        !name.is_empty() && !name.contains('/') && name != ".." && name != "." && name.len() > 33
    })
}

/// Checks that the store and the state directory are on one btrfs
/// filesystem, so that farms can be made with reflinks. The arguments are
/// the output of `findmnt --noheadings --output FSTYPE,UUID --target <dir>`
/// for `/nix/store` and for the state directory. Returns the problem, if
/// any.
pub fn layout_problem(store: &str, state: &str) -> Option<String> {
    let parse = |s: &str| {
        let mut f = s.split_whitespace();
        Some((f.next()?.to_owned(), f.next()?.to_owned()))
    };
    let (Some((store_fs, store_uuid)), Some((state_fs, state_uuid))) = (parse(store), parse(state))
    else {
        return Some(format!(
            "cannot read the filesystems of the store ({store:?}) and the state directory ({state:?})"
        ));
    };
    if store_fs != "btrfs" || state_fs != "btrfs" {
        return Some(format!(
            "the store is on {store_fs} and the state directory is on {state_fs}, not btrfs"
        ));
    }
    if store_uuid != state_uuid {
        return Some(format!(
            "the store ({store_uuid}) and the state directory ({state_uuid}) must be on one btrfs filesystem"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ignores_order_and_duplicates() {
        let a = key(["/nix/store/b", "/nix/store/a"]);
        assert_eq!(a, key(["/nix/store/a", "/nix/store/b", "/nix/store/a"]));
        assert_ne!(a, key(["/nix/store/a"]));
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn archive() {
        // Real output of nix 2.34.
        let json = r#"{"inputs":{"nixpkgs":{"inputs":{},"path":"/nix/store/6nwzgfvznah5rg192x0hnl5ykchdgi59-source"}},"path":"/nix/store/kzjgi5a0962aa53ydlwr59ffcvay3brw-source"}"#;
        assert_eq!(
            archive_paths(json).unwrap(),
            [
                "/nix/store/6nwzgfvznah5rg192x0hnl5ykchdgi59-source",
                "/nix/store/kzjgi5a0962aa53ydlwr59ffcvay3brw-source"
            ]
        );
        assert!(archive_paths("{}").is_err());
    }

    #[test]
    fn expiry() {
        let farms = [
            FarmUse {
                key: "old".into(),
                last_used: 0,
            },
            FarmUse {
                key: "busy".into(),
                last_used: 0,
            },
            FarmUse {
                key: "new".into(),
                last_used: 900,
            },
        ];
        let in_use = BTreeSet::from(["busy".to_owned()]);
        assert_eq!(expired(&farms, &in_use, 1000, 500), ["old"]);
        assert!(expired(&farms, &in_use, 1000, 1000).is_empty());
    }

    #[test]
    fn store_paths() {
        assert!(is_store_path(
            "/nix/store/6nwzgfvznah5rg192x0hnl5ykchdgi59-source"
        ));
        assert!(!is_store_path("/nix/store/"));
        assert!(!is_store_path("/nix/store/../etc"));
        assert!(!is_store_path(
            "/nix/store/6nwzgfvznah5rg192x0hnl5ykchdgi59-source/bin"
        ));
        assert!(!is_store_path("/tmp/x"));
    }

    #[test]
    fn layout() {
        // Real findmnt output: two subvolumes of one btrfs filesystem.
        let fs = "btrfs  8c0d1f5e-6b0e-4f36-9a59-3b0c5f8a7e21\n";
        assert_eq!(layout_problem(fs, fs), None);
        let other = "btrfs  2a4be7a1-1b3c-4d2e-8f00-0c1d2e3f4a5b\n";
        assert!(
            layout_problem(fs, other)
                .unwrap()
                .contains("one btrfs filesystem")
        );
        let ext4 = "ext4   8c0d1f5e-6b0e-4f36-9a59-3b0c5f8a7e21\n";
        assert!(layout_problem(ext4, ext4).unwrap().contains("not btrfs"));
        assert!(layout_problem("", fs).is_some());
    }
}
