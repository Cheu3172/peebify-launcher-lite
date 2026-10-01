// ------------ Folder Safety ------------
// Decides which folders setup may install into and which game folders uninstall may delete. Drive roots,
// Windows and Program Files folders and your own Documents or Desktop are refused so nothing important is lost.

use std::path::{Path, PathBuf};

use crate::consts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotSpecific,
    SystemOwned,
    NeedsAdmin,
    UserContent,
}

impl Refusal {
    pub fn message(&self) -> &'static str {
        match self {
            Refusal::NotSpecific => {
                "Choose a folder inside a drive, not the drive itself. \
                 Setup needs a folder of its own so uninstalling can clean up after it."
            }
            Refusal::SystemOwned => {
                "That folder belongs to Windows. Pick somewhere else, such as \
                 a folder on another drive."
            }
            Refusal::NeedsAdmin => {
                "Peebify installs just for you, so it cannot write to Program Files. \
                 The default location, or any folder you own, will work."
            }
            Refusal::UserContent => {
                "That is one of your own folders, and uninstalling Peebify would try \
                 to remove it. Pick a new folder. Adding \"Peebify Launcher\" to the \
                 end is enough."
            }
        }
    }
}

const USER_FOLDER_NAMES: [&str; 8] = [
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Videos",
    "Music",
    "Saved Games",
    "Favorites",
];

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

fn strip_verbatim(p: &Path) -> PathBuf {
    let Some(raw) = p.to_str() else {
        return p.to_path_buf();
    };
    let slashed = raw.replace('/', "\\");
    if let Some(rest) = slashed.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    PathBuf::from(slashed.strip_prefix(r"\\?\").unwrap_or(&slashed))
}

fn lexical(p: &Path) -> PathBuf {
    let plain = strip_verbatim(p);
    std::path::absolute(&plain).unwrap_or(plain)
}

fn resolved(p: &Path) -> PathBuf {
    let lexical = lexical(p);
    let mut tail = Vec::new();
    let mut cur = lexical.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(cur) {
            let mut out = strip_verbatim(&real);
            out.extend(tail.iter().rev());
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                cur = parent;
            }
            _ => return lexical.clone(),
        }
    }
}

fn path_key(p: &Path) -> Vec<String> {
    resolved(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

pub fn normalize_install_dir(dir: &Path) -> PathBuf {
    if dir.is_absolute() {
        lexical(dir)
    } else {
        dir.to_path_buf()
    }
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

pub fn is_strictly_inside(path: &Path, root: &Path) -> bool {
    let path = path_key(path);
    let root = path_key(root);
    path.len() > root.len() && path.starts_with(&root)
}

fn system_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(windir) = env_dir("SystemRoot").or_else(|| env_dir("windir")) {
        dirs.push(windir);
    }
    dirs
}

fn program_dirs() -> Vec<PathBuf> {
    ["ProgramFiles", "ProgramFiles(x86)", "ProgramData"]
        .into_iter()
        .filter_map(env_dir)
        .collect()
}

fn user_content_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut onedrive_roots: Vec<PathBuf> = ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"]
        .into_iter()
        .filter_map(env_dir)
        .collect();
    if let Some(profile) = env_dir("USERPROFILE") {
        dirs.extend(USER_FOLDER_NAMES.iter().map(|name| profile.join(name)));
        onedrive_roots.push(profile.join("OneDrive"));
        dirs.push(profile);
    }
    for root in onedrive_roots {
        dirs.extend(USER_FOLDER_NAMES.iter().map(|name| root.join(name)));
        dirs.push(root);
    }
    dirs.extend(crate::win::user_known_folders());
    for key in ["APPDATA", "LOCALAPPDATA", "PUBLIC"] {
        if let Some(dir) = env_dir(key) {
            dirs.push(dir);
        }
    }
    if let Some(data) = consts::user_data_dir() {
        dirs.push(data);
    }
    if let Some(cache) = consts::app_cache_dir() {
        dirs.push(cache);
    }
    dirs
}

pub fn validate_install_dir(dir: &Path) -> Result<(), Refusal> {
    if !dir.is_absolute() {
        return Err(Refusal::NotSpecific);
    }
    let target = path_key(dir);
    if target == path_key(&consts::default_install_dir()) {
        return Ok(());
    }
    if target.len() < 3 {
        return Err(Refusal::NotSpecific);
    }
    let within_any = |dirs: Vec<PathBuf>| dirs.iter().any(|p| target.starts_with(&path_key(p)));
    if within_any(system_dirs()) {
        return Err(Refusal::SystemOwned);
    }
    if within_any(program_dirs()) {
        return Err(Refusal::NeedsAdmin);
    }
    if user_content_dirs().iter().any(|p| path_key(p) == target) {
        return Err(Refusal::UserContent);
    }
    Ok(())
}

pub fn is_default_install_dir(dir: &Path) -> bool {
    same_path(dir, &consts::default_install_dir())
}

pub fn is_foreign_non_empty(dir: &Path) -> bool {
    if dir.join(consts::INSTALL_MANIFEST_NAME).exists() {
        return false;
    }
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

pub fn may_claim_existing_dir(dir: &Path) -> bool {
    if dir.join(consts::INSTALL_MANIFEST_NAME).exists() {
        return true;
    }
    let empty = std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    empty
        && dir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(consts::PRODUCT_NAME))
}

pub fn safe_to_remove_game_dir(dir: &Path) -> bool {
    if !dir.is_absolute() {
        return false;
    }
    let target = path_key(dir);
    if target.len() < 3 {
        return false;
    }
    let mut protected: Vec<PathBuf> = Vec::new();
    protected.extend(system_dirs());
    protected.extend(program_dirs());
    protected.extend(user_content_dirs());
    protected.push(consts::default_install_dir());
    !protected.iter().any(|p| path_key(p).starts_with(&target))
}

pub fn in_steam_library(dir: &Path) -> bool {
    let named = |p: &Path, name: &str| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
    };
    dir.ancestors().any(|current| {
        named(current, "common")
            && current.parent().is_some_and(|parent| named(parent, "steamapps"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> PathBuf {
        env_dir("USERPROFILE").expect("USERPROFILE is set")
    }

    fn slashed(p: &Path) -> PathBuf {
        PathBuf::from(p.to_string_lossy().replace('\\', "/"))
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "peebify-paths-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch dir");
        root
    }

    #[test]
    fn strip_verbatim_handles_prefixes_and_slashes() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\Games\Peebify")),
            PathBuf::from(r"C:\Games\Peebify")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\server\share\x")),
            PathBuf::from(r"\\server\share\x")
        );
        assert_eq!(
            strip_verbatim(Path::new("C:/Games/Peebify")),
            PathBuf::from(r"C:\Games\Peebify")
        );
    }

    #[test]
    fn same_path_ignores_case_separators_and_dot_segments() {
        let base = r"C:\PeebifyTestMissing\Peebify";
        for other in [
            r"c:\peebifytestmissing\peebify\",
            "C:/PeebifyTestMissing/Peebify",
            r"C:\PeebifyTestMissing\x\..\Peebify",
            r"C:\PeebifyTestMissing\.\Peebify",
            r"\\?\C:\PeebifyTestMissing\Peebify",
            r"C:\PeebifyTestMissing\\Peebify",
        ] {
            assert!(same_path(Path::new(base), Path::new(other)), "{other}");
        }
        assert!(!same_path(
            Path::new(base),
            Path::new(r"C:\PeebifyTestMissing\Peebify Launcher")
        ));
    }

    #[test]
    fn strictly_inside_needs_a_real_child() {
        let root = scratch("inside");
        std::fs::create_dir_all(root.join("sub")).expect("create test dir");
        assert!(is_strictly_inside(&root.join("sub").join("x.dll"), &root));
        assert!(is_strictly_inside(&slashed(&root.join("Missing")), &root));
        assert!(is_strictly_inside(&root.join("SUB"), &slashed(&root)));
        assert!(!is_strictly_inside(&root, &root));
        assert!(!is_strictly_inside(&root.join("sub").join(".."), &root));
        assert!(!is_strictly_inside(&root.join("..").join("elsewhere"), &root));
        assert!(!is_strictly_inside(&root.with_extension("x").join("a"), &root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn normalize_install_dir_resolves_lexically() {
        assert_eq!(
            normalize_install_dir(Path::new("C:/Games/x/../Peebify Launcher")),
            PathBuf::from(r"C:\Games\Peebify Launcher")
        );
        assert_eq!(
            normalize_install_dir(Path::new("Peebify Launcher")),
            PathBuf::from("Peebify Launcher")
        );
    }

    #[test]
    fn user_folders_are_refused_however_they_are_written() {
        let profile = profile();
        let music = profile.join("Music");
        for dir in [
            slashed(&music),
            profile.join("x").join("..").join("Music"),
            PathBuf::from(format!(r"\\?\{}", music.display())),
            music,
            slashed(&profile),
            profile.join("OneDrive").join("Pictures"),
        ] {
            assert_eq!(
                validate_install_dir(&dir),
                Err(Refusal::UserContent),
                "{}",
                dir.display()
            );
        }
    }

    #[test]
    fn known_folders_are_refused() {
        for dir in crate::win::user_known_folders() {
            assert!(validate_install_dir(&dir).is_err(), "{}", dir.display());
            assert!(
                validate_install_dir(&slashed(&dir)).is_err(),
                "{}",
                dir.display()
            );
        }
    }

    #[test]
    fn folder_inside_a_user_folder_is_allowed() {
        let dir = profile().join("Music").join(consts::PRODUCT_NAME);
        assert_eq!(validate_install_dir(&dir), Ok(()));
    }

    #[test]
    fn drive_roots_and_relative_paths_are_refused() {
        for raw in [
            r"C:\",
            "C:/",
            r"C:\x\..",
            r"C:\Games\..\.",
            r"\\?\C:\",
            "Peebify",
        ] {
            assert_eq!(
                validate_install_dir(Path::new(raw)),
                Err(Refusal::NotSpecific),
                "{raw}"
            );
        }
    }

    #[test]
    fn program_files_is_refused_with_slashes() {
        let Some(program_files) = env_dir("ProgramFiles") else {
            return;
        };
        let dir = slashed(&program_files.join("Peebify"));
        assert_eq!(validate_install_dir(&dir), Err(Refusal::NeedsAdmin));
    }

    #[test]
    fn default_install_dir_is_accepted_in_any_spelling() {
        let default = consts::default_install_dir();
        assert_eq!(validate_install_dir(&slashed(&default)), Ok(()));
        assert!(is_default_install_dir(&slashed(&default)));
    }

    #[test]
    fn game_dir_removal_refuses_protected_ancestors() {
        let profile = profile();
        assert!(!safe_to_remove_game_dir(&slashed(&profile)));
        assert!(!safe_to_remove_game_dir(
            &profile.join("Games").join("..")
        ));
        assert!(!safe_to_remove_game_dir(Path::new("C:/")));
        assert!(safe_to_remove_game_dir(
            &profile.join("PeebifyTestMissing").join("Some Game")
        ));
    }

    #[test]
    fn steam_library_is_detected_from_any_ancestor() {
        assert!(in_steam_library(Path::new(
            "D:/SteamLibrary/steamapps/common/Wuthering Waves"
        )));
        assert!(in_steam_library(Path::new(
            "C:\\Program Files (x86)\\Steam\\SteamApps\\Common\\Game\\Client"
        )));
        assert!(!in_steam_library(Path::new("D:/Games/common/Game")));
        assert!(!in_steam_library(Path::new("D:/steamapps/Game")));
        assert!(!in_steam_library(Path::new("D:/Games/Wuthering Waves")));
    }

    #[test]
    fn existing_dir_is_claimed_only_when_named_for_the_product_or_ours() {
        let root = scratch("claim");
        let named = root.join(consts::PRODUCT_NAME);
        let music = root.join("Music");
        let busy = root.join("Busy").join(consts::PRODUCT_NAME);
        let ours = root.join("Ours");
        for dir in [&named, &music, &busy, &ours] {
            std::fs::create_dir_all(dir).expect("create test dir");
        }
        std::fs::write(busy.join("notes.txt"), b"x").expect("write test file");
        std::fs::write(ours.join(consts::INSTALL_MANIFEST_NAME), b"{}").expect("write manifest");

        assert!(may_claim_existing_dir(&named));
        assert!(!may_claim_existing_dir(&music));
        assert!(!may_claim_existing_dir(&busy));
        assert!(may_claim_existing_dir(&ours));

        let _ = std::fs::remove_dir_all(&root);
    }
}
