// ------------ Game Path Checks ------------
// Figures out where a game really lives from the folder someone picked, looking a few levels in each direction for its executable, and decides if that folder is acceptable.
// It also holds the Windows path length budget, since some games break when installed too deep.
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::game_profiles;

pub struct Validation {
    pub is_valid: bool,
    pub error: Option<String>,
    pub resolved_path: Option<String>,
}

fn file_exists(path: &Path) -> bool {
    path.exists()
}

fn resolve(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

const WALK_DIR_BUDGET: usize = 5_000;
const WHOLE_DRIVE: &str = "Pick the game's own folder, not a whole drive.";

fn is_volume_root(path: &Path) -> bool {
    resolve(path).parent().is_none()
}

fn skipped_dir(entry: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN_OR_SYSTEM: u32 = 0x2 | 0x4;
    if entry
        .metadata()
        .is_ok_and(|m| m.file_attributes() & HIDDEN_OR_SYSTEM != 0)
    {
        return true;
    }
    std::env::var_os("SystemRoot")
        .is_some_and(|root| entry.path().as_os_str().eq_ignore_ascii_case(&root))
}

fn subdirs(dir: &Path, budget: &mut usize) -> Option<Vec<PathBuf>> {
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    Some(
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false) && !skipped_dir(e))
            .map(|e| e.path())
            .collect(),
    )
}

pub fn find_game_install_root(
    game_path: &Path,
    executable_name: &str,
    max_depth: u32,
) -> Option<PathBuf> {
    if executable_name.is_empty() {
        return None;
    }
    if file_exists(&game_path.join(executable_name)) {
        return Some(game_path.to_path_buf());
    }
    if is_volume_root(game_path) {
        return None;
    }

    fn walk(
        dir: &Path,
        exe: &str,
        depth: u32,
        max_depth: u32,
        budget: &mut usize,
    ) -> Option<PathBuf> {
        if depth > max_depth {
            return None;
        }
        let entries = subdirs(dir, budget)?;

        for sub in &entries {
            if file_exists(&sub.join(exe)) {
                return Some(sub.clone());
            }
        }
        for sub in &entries {
            if let Some(found) = walk(sub, exe, depth + 1, max_depth, budget) {
                return Some(found);
            }
        }
        None
    }

    let mut budget = WALK_DIR_BUDGET;
    walk(game_path, executable_name, 1, max_depth, &mut budget)
}

pub fn find_install_root_by_marker(
    game_path: &Path,
    marker_parts: &[String],
    max_depth: u32,
) -> Option<PathBuf> {
    if marker_parts.is_empty() {
        return None;
    }

    let marker_exists = |root: &Path| {
        let mut p = root.to_path_buf();
        for part in marker_parts {
            p.push(part);
        }
        file_exists(&p)
    };

    let mut dir = resolve(game_path);
    for _ in 0..5 {
        if marker_exists(&dir) {
            return Some(dir);
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => break,
        }
    }

    if is_volume_root(game_path) {
        return None;
    }

    fn walk(
        dir: &Path,
        depth: u32,
        max_depth: u32,
        marker_exists: &dyn Fn(&Path) -> bool,
        budget: &mut usize,
    ) -> Option<PathBuf> {
        if depth > max_depth {
            return None;
        }
        if marker_exists(dir) {
            return Some(dir.to_path_buf());
        }
        if depth == max_depth {
            return None;
        }
        for sub in subdirs(dir, budget)? {
            if let Some(found) = walk(&sub, depth + 1, max_depth, marker_exists, budget) {
                return Some(found);
            }
        }
        None
    }

    let mut budget = WALK_DIR_BUDGET;
    walk(&resolve(game_path), 0, max_depth, &marker_exists, &mut budget)
}

pub fn resolve_launch(install_root: &Path, profile: &Value) -> (PathBuf, Vec<String>) {
    let candidates = game_profiles::launch_candidates(profile);
    let joined = |candidate: &game_profiles::LaunchCandidate| {
        candidate
            .parts
            .iter()
            .fold(install_root.to_path_buf(), |p, part| p.join(part))
    };
    for candidate in &candidates {
        let path = joined(candidate);
        if file_exists(&path) {
            return (path, candidate.args.clone());
        }
    }
    match candidates.first() {
        Some(first) => (joined(first), first.args.clone()),
        None => (
            install_root.join(game_profiles::executable_name(profile)),
            Vec::new(),
        ),
    }
}

pub fn launch_executable_path(install_root: &Path, profile: &Value) -> PathBuf {
    resolve_launch(install_root, profile).0
}

pub const WINDOWS_MAX_PATH: usize = 259;

pub fn max_install_root_len(profile: &Value) -> usize {
    WINDOWS_MAX_PATH.saturating_sub(game_profiles::max_path_tail(profile))
}

pub fn fits_path_budget(root: &Path, profile: &Value) -> bool {
    root_len(root) <= max_install_root_len(profile)
}

pub(crate) fn root_len(root: &Path) -> usize {
    root.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .encode_utf16()
        .count()
}

pub fn path_budget_error(root: &Path, profile: &Value) -> Option<String> {
    let limit = max_install_root_len(profile);
    let len = root_len(root);
    if len <= limit {
        return None;
    }
    Some(format!(
        "This folder is {} character{} too deep for {}. Windows limits the game to {WINDOWS_MAX_PATH}-character paths, and {} needs {} of those for the files it writes itself, so its folder has to be at most {limit} characters long (this one is {len}). Pick a shorter folder, such as one directly off C:\\.",
        len - limit,
        if len - limit == 1 { "" } else { "s" },
        game_profiles::display_name(profile),
        game_profiles::display_name(profile),
        game_profiles::max_path_tail(profile),
    ))
}

pub fn validate_game_path_for_profile(game_path: &str, profile: &Value) -> Validation {
    if game_path.is_empty() {
        return Validation {
            is_valid: false,
            error: Some("No path provided.".to_string()),
            resolved_path: None,
        };
    }
    let picked = Path::new(game_path);

    if let Some(marker) = game_profiles::install_root_marker(profile) {
        let Some(install_root) = find_install_root_by_marker(picked, &marker, 4) else {
            let error = if is_volume_root(picked) {
                WHOLE_DRIVE
            } else {
                "Could not find NTE game files in this folder or its parents."
            };
            return Validation {
                is_valid: false,
                error: Some(error.to_string()),
                resolved_path: None,
            };
        };
        let launch_exe = launch_executable_path(&install_root, profile);
        if !file_exists(&launch_exe) {
            let base = launch_exe
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            return Validation {
                is_valid: false,
                error: Some(format!(
                    "\"{base}\" not found under the detected install folder."
                )),
                resolved_path: None,
            };
        }
        let selected = resolve(picked);
        let resolved = resolve(&install_root);
        return Validation {
            is_valid: true,
            error: None,
            resolved_path: (resolved != selected)
                .then(|| install_root.to_string_lossy().to_string()),
        };
    }

    let exe = game_profiles::executable_name(profile);
    let Some(resolved) = find_game_install_root(picked, exe, 4) else {
        let error = if is_volume_root(picked) {
            WHOLE_DRIVE.to_string()
        } else {
            format!("\"{exe}\" not found in this folder or its subfolders.")
        };
        return Validation {
            is_valid: false,
            error: Some(error),
            resolved_path: None,
        };
    };
    let resolved_str = resolved.to_string_lossy().to_string();
    Validation {
        is_valid: true,
        error: None,
        resolved_path: (resolved_str != game_path).then_some(resolved_str),
    }
}

#[cfg(test)]
mod path_budget_tests {
    use super::*;

    const RE1999_WORST_TAIL: &str = concat!(
        r"reverse1999_Data\StreamingAssets\PersistentRoot\_tmp1\60001\Windows\",
        r"b506cfbcc5e4cd49f865962da80b7300\",
        "hotupdate_847690_49bb686f5d6b59c49f47158e0c30c633_",
        "7256a44a1d7e5767113affb11ad09e8d.zip",
    );

    #[test]
    fn the_budget_covers_the_path_the_game_actually_failed_on() {
        let profile = game_profiles::profile("re1999");
        let needed = RE1999_WORST_TAIL.len() + 1;
        assert!(
            needed <= game_profiles::max_path_tail(profile),
            "{} needs {needed} characters below its folder, but the budget is {}",
            game_profiles::display_name(profile),
            game_profiles::max_path_tail(profile),
        );
    }

    #[test]
    fn the_folder_that_broke_the_game_is_refused() {
        let profile = game_profiles::profile("re1999");
        let too_deep = Path::new(
            r"C:\Users\Alice\AppData\Local\Programs\Peebify Launcher\games\Reverse 1999",
        );

        assert!(!fits_path_budget(too_deep, profile));
        let message = path_budget_error(too_deep, profile).expect("a reason");
        assert!(message.contains("Reverse: 1999"), "unhelpful: {message}");

        let full = too_deep.join(RE1999_WORST_TAIL);
        assert!(full.to_string_lossy().chars().count() > WINDOWS_MAX_PATH);
    }

    #[test]
    fn a_short_folder_is_accepted_and_has_no_complaint() {
        let profile = game_profiles::profile("re1999");
        let fine = Path::new(r"C:\Users\Alice\Peebify Games\Reverse 1999");

        assert!(fits_path_budget(fine, profile));
        assert!(path_budget_error(fine, profile).is_none());
        assert!(
            fine.join(RE1999_WORST_TAIL)
                .to_string_lossy()
                .chars()
                .count()
                <= WINDOWS_MAX_PATH
        );
    }

    #[test]
    fn a_trailing_separator_does_not_change_the_verdict() {
        let profile = game_profiles::profile("re1999");
        let limit = max_install_root_len(profile);
        let exact = "a".repeat(limit);

        assert!(fits_path_budget(Path::new(&exact), profile));
        assert!(fits_path_budget(Path::new(&format!("{exact}\\")), profile));
        assert!(!fits_path_budget(Path::new(&format!("{exact}a")), profile));
    }

    #[test]
    fn characters_outside_the_bmp_count_as_two_path_units() {
        let profile = game_profiles::profile("re1999");
        let limit = max_install_root_len(profile);
        let ascii = "a".repeat(limit - 1);
        let with_emoji = format!("{ascii}\u{1F600}");

        assert_eq!(root_len(Path::new(&with_emoji)), limit + 1);
        assert!(!fits_path_budget(Path::new(&with_emoji), profile));
        assert!(path_budget_error(Path::new(&with_emoji), profile).is_some());
        assert!(fits_path_budget(Path::new(&format!("{ascii}\u{e9}")), profile));
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_root_is_not_searched_below() {
        assert!(is_volume_root(Path::new(r"C:\")));
        assert!(is_volume_root(Path::new(r"D:\")));
        assert!(is_volume_root(Path::new(r"\\server\share\")));
        assert!(!is_volume_root(Path::new(r"C:\Games")));
        assert!(!is_volume_root(Path::new(r"D:\Games\Wuthering Waves")));

        assert_eq!(find_game_install_root(Path::new(r"C:\"), "no-such-game.exe", 4), None);
        let validation =
            validate_game_path_for_profile(r"C:\", game_profiles::profile("re1999"));
        assert!(!validation.is_valid);
        assert_eq!(validation.error.as_deref(), Some(WHOLE_DRIVE));
    }

    #[test]
    fn a_game_below_the_picked_folder_is_still_found() {
        let root = std::env::temp_dir().join(format!("peebify-locate-{}", uuid::Uuid::new_v4()));
        let game = root.join("Games").join("Game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("game.exe"), b"x").unwrap();

        let found = find_game_install_root(&root, "game.exe", 4);
        let direct = find_game_install_root(&game, "game.exe", 4);
        let too_shallow = find_game_install_root(&root, "game.exe", 1);
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(found, Some(game.clone()));
        assert_eq!(direct, Some(game));
        assert_eq!(too_shallow, None);
    }

    #[test]
    fn a_game_without_a_declared_budget_still_gets_one() {
        let profile = game_profiles::profile("wuwa");
        assert_eq!(
            game_profiles::max_path_tail(profile),
            game_profiles::DEFAULT_MAX_PATH_TAIL
        );
        assert!(max_install_root_len(profile) > 0);
    }
}
