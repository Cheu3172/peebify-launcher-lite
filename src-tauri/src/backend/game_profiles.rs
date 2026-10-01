// ------------ Game Profiles ------------
// The list of supported games and everything the launcher needs to know about each one: names, executables, download sources, install type, mod support and so on.
// Other files look a game up here by id instead of hardcoding anything about it.
use std::sync::OnceLock;

use serde_json::{json, Value};

pub const GAME_IDS: [&str; 12] = [
    "wuwa", "zzz", "hsr", "nte", "endfield", "genshin", "hi3", "pgr", "gf2", "gf1", "re1999", "bd2",
];
pub const DEFAULT_GAME_ID: &str = "wuwa";
pub const VOICE_LANGUAGES: [&str; 4] = ["en-us", "ja-jp", "ko-kr", "zh-cn"];

pub fn is_known_game_id(id: &str) -> bool {
    GAME_IDS.contains(&id)
}

fn voice_map() -> &'static parking_lot::Mutex<std::collections::HashMap<String, String>> {
    static MAP: OnceLock<parking_lot::Mutex<std::collections::HashMap<String, String>>> =
        OnceLock::new();
    MAP.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

pub fn set_audio_language(profile_id: &str, language: &str) {
    let mut map = voice_map().lock();
    if VOICE_LANGUAGES.contains(&language) {
        log::info!("[voice] {profile_id} voice pack set to {language}");
        map.insert(profile_id.to_string(), language.to_string());
    } else {
        map.remove(profile_id);
    }
}

pub fn audio_language(profile_id: &str) -> String {
    voice_map()
        .lock()
        .get(profile_id)
        .cloned()
        .unwrap_or_else(|| super::download_engine::HOYO_AUDIO_LANGUAGE.to_string())
}

fn saved_voice_language(app: &tauri::AppHandle, key: &str) -> Option<String> {
    super::backend(app)
        .config
        .get(key)
        .as_str()
        .filter(|lang| VOICE_LANGUAGES.contains(lang))
        .map(str::to_string)
}

async fn voice_language_on_disk(
    profile: &Value,
    dir: &std::path::Path,
    build: Option<&super::sophon::Build>,
) -> Option<String> {
    let probe = dir.to_path_buf();
    let has_files = tauri::async_runtime::spawn_blocking(move || {
        std::fs::read_dir(&probe).is_ok_and(|mut entries| entries.next().is_some())
    })
    .await
    .unwrap_or(false);
    if !has_files {
        return None;
    }

    let fetched;
    let build = match build {
        Some(build) => build,
        None => {
            let fetch = async {
                let auth = super::sophon::cached_branch_auth_for_profile(profile).await?;
                super::sophon::fetch_build(&auth).await
            };
            match fetch.await {
                Ok(build) => {
                    fetched = build;
                    &fetched
                }
                Err(e) => {
                    log::warn!(
                        "[voice] {}: could not read the build to find the voice pack: {e}",
                        profile_id(profile)
                    );
                    return None;
                }
            }
        }
    };
    match super::sophon::detect_voice_language(dir, build, &VOICE_LANGUAGES).await {
        Ok(found) => found,
        Err(e) => {
            log::warn!(
                "[voice] {}: could not check the voice pack on disk: {e}",
                profile_id(profile)
            );
            None
        }
    }
}

pub(crate) async fn resolve_audio_language(
    app: &tauri::AppHandle,
    profile: &Value,
    dir: &std::path::Path,
    build: Option<&super::sophon::Build>,
) -> String {
    let id = profile_id(profile);
    if install_mode(profile) != Some("sophon") {
        return audio_language(id);
    }
    let key = format!("games.{id}.voicePackLanguage");
    if let Some(lang) = saved_voice_language(app, &key) {
        return lang;
    }

    let applied_dir = dir.to_path_buf();
    let recorded = tauri::async_runtime::spawn_blocking(move || {
        let applied = super::sophon::load_applied(&applied_dir)?;
        super::sophon::applied_voice_languages(&applied)
            .into_iter()
            .find(|lang| VOICE_LANGUAGES.contains(&lang.as_str()))
    })
    .await
    .ok()
    .flatten();
    let (found, source) = match recorded {
        Some(lang) => (Some(lang), "Peebify's install record"),
        None => (
            voice_language_on_disk(profile, dir, build).await,
            "the files on disk",
        ),
    };
    let Some(lang) = found else {
        return audio_language(id);
    };

    if let Some(saved) = saved_voice_language(app, &key) {
        return saved;
    }
    log::info!("[voice] {id}: found the {lang} voice pack from {source}.");
    super::config_channels::set_config_value(app, &key, json!(lang));
    set_audio_language(id, &lang);
    super::install_preview::invalidate_preview(id);
    use tauri::Emitter;
    let _ = app.emit("settings-changed", json!({ "key": key, "value": lang }));
    lang
}

fn content_tag_map() -> &'static parking_lot::Mutex<std::collections::HashMap<String, Vec<String>>>
{
    static MAP: OnceLock<parking_lot::Mutex<std::collections::HashMap<String, Vec<String>>>> =
        OnceLock::new();
    MAP.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

pub fn set_content_tags(profile_id: &str, tags: Option<Vec<String>>) {
    let mut map = content_tag_map().lock();
    match tags {
        Some(list) => {
            log::info!("[content] {profile_id} optional packs set to {list:?}");
            map.insert(profile_id.to_string(), list);
        }
        None => {
            map.remove(profile_id);
        }
    }
}

pub fn content_tags(profile_id: &str) -> Option<Vec<String>> {
    content_tag_map().lock().get(profile_id).cloned()
}

// ------------ Profile Table ------------
// The profile of every supported game, written out as one big JSON value that is built once on first use.
fn profiles() -> &'static Value {
    static PROFILES: OnceLock<Value> = OnceLock::new();
    PROFILES.get_or_init(|| {
        json!({
            "wuwa": {
                "id": "wuwa",
                "wallpaperSlug": "wuthering-waves",
                "displayName": "Wuthering Waves",
                "shortName": "Wuthering Waves",
                "apiClientKey": "wuwa",

                "supportsManagedInstall": true,
                "gameConfigUrl": "https://prod-alicdn-gamestarter.kurogame.com/launcher/game/50004_P7xcUZnEr1AXIGON25E6KjpOgTlVrg6e/G153/official/index.json",

                "executableName": "Wuthering Waves.exe",
                "clientProcessName": "Client-Win64-Shipping.exe",

                "steamAppId": "3513350",
                "steamAppIdFiles": [["Client", "Binaries", "Win64", "steam_appid.txt"]],

                "graphicsApiArgs": { "dx11": [], "dx12": ["-dx12"] },
                "graphicsApiDefault": "dx12",

                "resourceQualityArgs": { "sd": ["-krqlv=sd"], "hd": ["-krqlv=hd"], "uhd": ["-krqlv=uhd"] },
                "resourceQualityDefault": "hd",

                "mods": {
                    "variant": "wwmi",
                    "loaderMode": "inject",
                    "forceDx11Args": ["-dx11"],
                    "gameBananaId": 20357,
                    "modIniDefaults": {
                        "Rendering": { "texture_hash": 1, "track_texture_updates": 1, "track_region_hashes": 0, "allow_buffer_resize": 1 },
                        "System": { "dll_initialization_delay": 500 },
                        "Logging": { "show_warnings": 0 }
                    }
                },

                "communityTools": {
                    "official": [
                        {
                            "name": "Wuthering Waves Official",
                            "url": "https://wutheringwaves.kurogames.com/en/main"
                        },
                        {
                            "name": "Wuthering Waves News",
                            "url": "https://wutheringwaves.kurogames.com/en/main#news"
                        }
                    ],
                    "community": [
                        { "name": "Wuthering Waves Tracker", "url": "https://wuwatracker.com/" },
                        { "name": "Wuthering Waves Map", "url": "https://wuthering-waves-map.appsample.com/" },
                        { "name": "Wuthering Waves Builds", "url": "https://game8.co/games/Wuthering-Waves/archives/457465" }
                    ]
                },

                "socialUrls": {
                    "discord": "https://discord.gg/wutheringwaves",
                    "youtube": "https://www.youtube.com/c/WutheringWaves",
                    "x": "https://twitter.com/Wuthering_Waves",
                    "lunite": "https://payment.kurogame-service.com/pay/wutheringwaves/"
                }
            },

            "zzz": {
                "id": "zzz",
                "wallpaperSlug": "zenless-zone-zero",
                "displayName": "Zenless Zone Zero",
                "shortName": "Zenless Zone Zero",
                "apiClientKey": "zzz",

                "supportsManagedInstall": true,
                "installMode": "sophon",
                "hoyoBiz": "nap_global",
                "hoyoGameId": "U5hbdsT9W7",

                "executableName": "ZenlessZoneZero.exe",
                "clientProcessName": "ZenlessZoneZero.exe",

                "steamAppId": "4162040",
                "steamAppIdFiles": [["steam_appid.txt"]],

                "graphicsApiArgs": { "dx11": [], "dx12": ["-use-d3d12"] },
                "graphicsApiDefault": "dx11",

                "mods": {
                    "variant": "zzmi",
                    "loaderMode": "hook",
                    "forceDx11Args": ["-use-d3d11"],
                    "gameBananaId": 19567,
                    "modIniDefaults": {
                        "Rendering": { "texture_hash": 0, "track_texture_updates": 0, "track_region_hashes": 0, "allow_buffer_resize": 1 },
                        "System": { "dll_initialization_delay": 0 },
                        "Logging": { "show_warnings": 0 }
                    }
                },

                "communityTools": {
                    "official": [
                        {
                            "name": "Zenless Zone Zero Official",
                            "url": "https://zenless.hoyoverse.com/"
                        },
                        {
                            "name": "Zenless Zone Zero News",
                            "url": "https://zenless.hoyoverse.com/news"
                        }
                    ],
                    "community": [
                        { "name": "Zenless Zone Zero Builds", "url": "https://docs.google.com/spreadsheets/d/e/2PACX-1vTj2PaPq6Py_1B5fsOPj_Moc-tN_7mut7fICczI6lz1njyEIAInTnfB7lAraX4pYCRGNbaHGlIbFZ90/pubhtml" },
                        { "name": "Interknot Network", "url": "https://interknot-network.com/" }
                    ]
                },

                "luniteUsesHoyolabIcon": true,
                "socialUrls": {
                    "discord": "https://discord.com/invite/zenlesszonezero",
                    "youtube": "https://www.youtube.com/@ZZZ_Official",
                    "x": "https://twitter.com/ZZZ_EN",
                    "lunite": "https://www.hoyolab.com/accountCenter/postList?id=219270333"
                }
            },

            "hsr": {
                "id": "hsr",
                "wallpaperSlug": "honkai-star-rail",
                "displayName": "Honkai: Star Rail",
                "shortName": "Honkai: Star Rail",
                "apiClientKey": "hsr",

                "supportsManagedInstall": true,
                "installMode": "sophon",
                "hoyoBiz": "hkrpg_global",
                "hoyoGameId": "4ziysqXOQ8",

                "executableName": "StarRail.exe",
                "clientProcessName": "StarRail.exe",

                "mods": {
                    "variant": "srmi",
                    "loaderMode": "hook",
                    "forceDx11Args": [],
                    "gameBananaId": 18366,
                    "modIniDefaults": {
                        "Rendering": { "texture_hash": 0, "track_texture_updates": 0, "track_region_hashes": 0, "track_implicit_index_buffers": 1, "allow_buffer_resize": 1 },
                        "System": { "dll_initialization_delay": 0 },
                        "Logging": { "show_warnings": 0 }
                    }
                },

                "communityTools": {
                    "official": [
                        {
                            "name": "Honkai: Star Rail Official",
                            "url": "https://hsr.hoyoverse.com/"
                        },
                        {
                            "name": "Honkai: Star Rail News",
                            "url": "https://hsr.hoyoverse.com/en/news"
                        }
                    ],
                    "community": [
                        { "name": "Honkai: Star Rail Builds", "url": "https://game8.co/games/Honkai-Star-Rail/archives/404256" }
                    ]
                },

                "luniteUsesHoyolabIcon": true,
                "socialUrls": {
                    "discord": "https://discord.gg/honkaistarrail",
                    "youtube": "https://www.youtube.com/@HonkaiStarRail",
                    "x": "https://twitter.com/HonkaiStarRail",
                    "lunite": "https://www.hoyolab.com/accountCenter/postList?id=172534910"
                }
            },

            "nte": {
                "id": "nte",
                "wallpaperSlug": "neverness-to-everness",
                "displayName": "Neverness to Everness",
                "shortName": "Neverness to Everness",
                "apiClientKey": "nte",

                "supportsManagedInstall": true,
                "installMode": "netease",
                "showLuniteSocial": false,

                "nteResUrls": [
                    "https://ntecdn1.perfectworld.com/clientRes",
                    "https://ntecdn2.perfectworld.com/clientRes"
                ],
                "nteLauncherUrls": [
                    "https://ntecdn1.perfectworld.com/hd/publish_PC/launcher",
                    "https://ntecdn2.perfectworld.com/hd/publish_PC/launcher"
                ],
                "nteBranch": "publish_PC",
                "nteAppId": "3000001",

                "installRootMarker": ["Client", "WindowsNoEditor", "HT", "Binaries", "Win64", "HTGame.exe"],
                "launchCandidates": [
                    { "path": ["NTEGlobal", "NTEGlobalGame.exe"], "args": ["/launcher", "/directly"] },
                    { "path": ["Client", "WindowsNoEditor", "HT", "Binaries", "Win64", "HTGame.exe"] }
                ],
                "executableName": "NTEGlobalGame.exe",
                "clientProcessName": "HTGame.exe",
                "exitCompanions": ["NTEGlobalGame.exe", "NTEGlobalBrowser.exe", "NTEGlobalWebBooster.exe"],
                "hiddenFrontEnd": ["NTEGlobalGame.exe", "NTEGlobalBrowser.exe", "NTEGlobalWebBooster.exe"],

                "communityTools": {
                    "official": [
                        {
                            "name": "NTE Official",
                            "url": "https://nte.perfectworld.com/en/"
                        },
                        {
                            "name": "NTE News",
                            "url": "https://nte.perfectworld.com/en/article/news/gamenews/index.html"
                        }
                    ],
                    "community": [
                        { "name": "Neverness to Everness Builds", "url": "https://game8.co/games/Neverness-to-Everness/archives/596186" }
                    ]
                },

                "socialUrls": {}
            },

            "endfield": {
                "id": "endfield",
                "wallpaperSlug": "arknights-endfield",
                "displayName": "Arknights: Endfield",
                "shortName": "Endfield",
                "apiClientKey": "endfield",

                "supportsManagedInstall": true,
                "installMode": "hypergryph",
                "showLuniteSocial": false,

                "hgApiUrl": "https://launcher.gryphline.com/api/proxy/batch_proxy",
                "hgWebApiUrl": "https://launcher.gryphline.com/api/proxy/web/batch_proxy",
                "hgAppCode": "YDUTE5gscDZ229CW",
                "hgChannel": "6",
                "hgSubChannel": "6",
                "hgSeq": "3",
                "hgLanguage": "en-us",

                "executableName": "Endfield.exe",
                "clientProcessName": "Endfield.exe",

                "mods": {
                    "variant": "efmi",
                    "loaderMode": "inject",
                    "forceDx11Args": ["-force-d3d11"],
                    "gameBananaId": 21842,
                    "modIniDefaults": {
                        "Rendering": { "texture_hash": 0, "track_texture_updates": 0, "track_region_hashes": 1, "track_implicit_index_buffers": 1, "allow_buffer_resize": 0 },
                        "System": { "dll_initialization_delay": 0 },
                        "Logging": { "show_warnings": 0 }
                    }
                },

                "communityTools": {
                    "official": [
                        {
                            "name": "Arknights: Endfield Official",
                            "url": "https://endfield.gryphline.com/en-us#home"
                        },
                        {
                            "name": "Arknights: Endfield News",
                            "url": "https://endfield.gryphline.com/en-us#notice"
                        }
                    ],
                    "community": []
                },

                "socialUrls": {
                    "discord": "https://discord.com/invite/akendfield",
                    "youtube": "https://youtube.com/@arknightsendfielden",
                    "x": "https://x.com/AKEndfield"
                }
            },

            "genshin": {
                "id": "genshin",
                "wallpaperSlug": "genshin-impact",
                "displayName": "Genshin Impact",
                "shortName": "Genshin Impact",
                "apiClientKey": "genshin",

                "supportsManagedInstall": true,
                "installMode": "sophon",
                "hoyoBiz": "hk4e_global",
                "hoyoGameId": "gopR6Cufr3",

                "executableName": "GenshinImpact.exe",
                "clientProcessName": "GenshinImpact.exe",

                "fpsUnlock": true,

                "mods": {
                    "variant": "gimi",
                    "loaderMode": "hook",
                    "forceDx11Args": [],
                    "gameBananaId": 8552,
                    "modIniDefaults": {
                        "Rendering": { "texture_hash": 0, "track_texture_updates": 0, "track_region_hashes": 0, "allow_buffer_resize": 1 },
                        "System": { "dll_initialization_delay": 0 },
                        "Logging": { "show_warnings": 0 }
                    }
                },

                "communityTools": {
                    "official": [
                        {
                            "name": "Genshin Impact Official",
                            "url": "https://genshin.hoyoverse.com/"
                        },
                        {
                            "name": "Genshin Impact News",
                            "url": "https://genshin.hoyoverse.com/en/news"
                        }
                    ],
                    "community": [
                        { "name": "Genshin Interactive Map", "url": "https://act.hoyolab.com/ys/app/interactive-map/index.html" },
                        { "name": "Genshin Impact Builds", "url": "https://genshin-builds.com/" }
                    ]
                },

                "luniteUsesHoyolabIcon": true,
                "socialUrls": {
                    "discord": "https://discord.gg/genshinimpact",
                    "youtube": "https://www.youtube.com/@GenshinImpact",
                    "x": "https://twitter.com/GenshinImpact",
                    "lunite": "https://www.hoyolab.com/accountCenter/postList?id=143134221"
                }
            },

            "hi3": {
                "id": "hi3",
                "wallpaperSlug": "honkai-impact-3rd",
                "displayName": "Honkai Impact 3rd",
                "shortName": "Honkai Impact 3rd",
                "apiClientKey": "hi3",

                "supportsManagedInstall": true,
                "installMode": "sophon",
                "hoyoGameId": "5TIVvvcwtM",
                "hoyoBiz": "bh3_global",

                "executableName": "BH3.exe",
                "clientProcessName": "BH3.exe",

                "mods": {
                    "variant": "himi",
                    "loaderMode": "hook",
                    "forceDx11Args": [],
                    "gameBananaId": 10349,
                    "experimental": true,
                    "modIniDefaults": {
                        "Rendering": { "texture_hash": 0, "track_texture_updates": 0, "track_region_hashes": 0, "allow_buffer_resize": 1 },
                        "System": { "dll_initialization_delay": 0 },
                        "Logging": { "show_warnings": 0 }
                    }
                },

                "steamAppId": "1671200",

                "communityTools": {
                    "official": [
                        {
                            "name": "Honkai Impact 3rd Official",
                            "url": "https://honkaiimpact3.hoyoverse.com/"
                        },
                        {
                            "name": "Honkai Impact 3rd News",
                            "url": "https://honkaiimpact3.hoyoverse.com/news"
                        }
                    ],
                    "community": [
                        { "name": "Honkai Impact 3rd Builds", "url": "https://game8.co/games/Honkai-Impact-3rd" }
                    ]
                },

                "luniteUsesHoyolabIcon": true,
                "socialUrls": {
                    "discord": "https://discord.gg/honkaiimpact3",
                    "youtube": "https://www.youtube.com/@HonkaiImpact3rd",
                    "x": "https://twitter.com/HonkaiImpact3",
                    "lunite": "https://www.hoyolab.com/accountCenter/postList?id=73565430"
                }
            },

            "pgr": {
                "id": "pgr",
                "wallpaperSlug": "punishing-gray-raven",
                "displayName": "Punishing: Gray Raven",
                "shortName": "Punishing: Gray Raven",
                "apiClientKey": "pgr",

                "supportsManagedInstall": true,
                "gameConfigUrl": "https://prod-alicdn-gamestarter.kurogame.com/launcher/game/G143/50015_LWdk9D2Ep9mpJmqBZZkcPBU2YNraEWBQ/index.json",
                "newsSource": "pgr",

                "executableName": "PGR.exe",
                "clientProcessName": "PGR.exe",

                "steamAppId": "4125930",

                "graphicsApiArgs": { "dx11": ["-force-d3d11"], "dx12": ["-force-d3d12"] },
                "graphicsApiDefault": "dx11",

                "showLuniteSocial": false,
                "communityTools": {
                    "official": [
                        {
                            "name": "Punishing: Gray Raven Official",
                            "url": "https://pgr.kurogame.net/"
                        },
                        {
                            "name": "Punishing: Gray Raven News",
                            "url": "https://pgr.kurogame.net/news"
                        }
                    ],
                    "community": [
                        { "name": "Punishing: Gray Raven Builds", "url": "https://grayravens.com/wiki/Guides" }
                    ]
                },

                "socialUrls": {
                    "discord": "https://discord.gg/pgr",
                    "youtube": "https://www.youtube.com/@PunishingGrayRaven",
                    "x": "https://twitter.com/PGR_Global"
                }
            },

            "gf2": {
                "id": "gf2",
                "wallpaperSlug": "girls-frontline-2",
                "displayName": "Girls' Frontline 2: Exilium",
                "shortName": "Girls' Frontline 2",
                "apiClientKey": "gf2",

                "supportsManagedInstall": true,
                "installMode": "gf2",
                "showLuniteSocial": false,

                "gf2ConfigUrl": "https://gf2-launcher-us.sunborngame.com/flexible_config?version=1.0.2",
                "gf2ClientPackage": "GF2_Exilium_Origin.zip",

                "executableName": "GF2_Exilium.exe",
                "clientProcessName": "GF2_Exilium.exe",

                "steamAppId": "3347400",

                "communityTools": {
                    "official": [
                        {
                            "name": "Girls' Frontline 2 Official",
                            "url": "https://gf2exilium.sunborngame.com/"
                        },
                        {
                            "name": "Girls' Frontline 2 News",
                            "url": "https://gf2exilium.sunborngame.com/main/noticeMore"
                        }
                    ],
                    "community": [
                        { "name": "IOP Wiki", "url": "https://iopwiki.com/wiki/Girls%27_Frontline_2:_Exilium" },
                        { "name": "Girls' Frontline 2 Builds", "url": "https://game8.co/games/Girls-Frontline-2-Exilium" }
                    ]
                },

                "socialUrls": {
                    "discord": "https://discord.gg/gfl2",
                    "youtube": "https://www.youtube.com/@GFL2EXILIUM",
                    "x": "https://x.com/gfl2exilium_en"
                }
            },

            "gf1": {
                "id": "gf1",
                "wallpaperSlug": "girls-frontline",
                "displayName": "Girls' Frontline",
                "shortName": "Girls' Frontline",
                "apiClientKey": "gf1",
                "supportsManagedInstall": false,
                "newsSource": "gf1",
                "showLuniteSocial": false,

                "executableName": "GirlsFrontLine.exe",
                "clientProcessName": "GirlsFrontLine.exe",
                "exitCompanions": ["ZFGameBrowser.exe"],

                "steamAppId": "3887700",

                "communityTools": {
                    "official": [
                        {
                            "name": "Girls' Frontline Official",
                            "url": "https://gf.sunborngame.com/"
                        },
                        {
                            "name": "Girls' Frontline News",
                            "url": "https://store.steampowered.com/news/app/3887700"
                        }
                    ],
                    "community": [
                        { "name": "IOP Wiki", "url": "https://iopwiki.com/wiki/Girls%27_Frontline" },
                        { "name": "Girls' Frontline Guides", "url": "https://iopwiki.com/wiki/Category:Guides" }
                    ]
                },

                "socialUrls": {
                    "x": "https://x.com/GirlsFrontlineE"
                }
            },

            "re1999": {
                "id": "re1999",
                "wallpaperSlug": "reverse-1999",
                "displayName": "Reverse: 1999",
                "shortName": "Reverse: 1999",
                "apiClientKey": "re1999",

                "supportsManagedInstall": true,
                "installMode": "bluepoch",
                "showLuniteSocial": false,

                "maxPathTail": 200,

                "bpHotUpdateUrls": [
                    "https://hotupdate-hw.sl916.com",
                    "https://hotupdate-bak-hw.sl916.com"
                ],
                "bpActivityUrls": [
                    "https://launcher-hw.sl916.com",
                    "https://launcher-bak-hw.sl916.com"
                ],
                "bpGameId": "60001",
                "bpChannelId": 200,
                "bpSubChannelId": "6001",
                "bpOsType": 2,
                "bpEnvType": 4,
                "bpLang": "en",
                "bpNewsUrl": "https://re1999.bluepoch.com/",

                "executableName": "reverse1999.exe",
                "clientProcessName": "reverse1999.exe",
                "exitCompanions": ["ZFGameBrowser.exe", "UnityCrashHandler64.exe"],

                "communityTools": {
                    "official": [
                        {
                            "name": "Reverse: 1999 Official",
                            "url": "https://re1999.bluepoch.com/"
                        },
                        {
                            "name": "Reverse: 1999 News",
                            "url": "https://re1999.bluepoch.com/#news"
                        }
                    ],
                    "community": [
                        { "name": "Reverse: 1999 Wiki", "url": "https://reverse1999.fandom.com/wiki/Reverse:1999_Wiki" },
                        { "name": "Reverse: 1999 Builds", "url": "https://game8.co/games/Reverse-1999" }
                    ]
                },

                "socialUrls": {
                    "discord": "https://discord.gg/reverse1999",
                    "youtube": "https://www.youtube.com/@Reverse1999",
                    "x": "https://twitter.com/Reverse1999_GL"
                }
            },

            "bd2": {
                "id": "bd2",
                "wallpaperSlug": "brown-dust-2",
                "displayName": "Brown Dust II",
                "shortName": "Brown Dust II",
                "apiClientKey": "bd2",

                "supportsManagedInstall": true,
                "installMode": "bd2",
                "showLuniteSocial": false,

                "bdStarterConfigUrl": "https://pc.bd2.pmang.cloud/browndust2starter/starter/config/launcher_g.json",
                "bdGameKey": "10000002",
                "bdNewsApiUrl": "https://webapi.browndust2.com/api",
                "bdNewsUrl": "https://www.browndust2.com",
                "bdNewsLocale": "en-us",

                "executableName": "BrownDust II.exe",
                "clientProcessName": "BrownDust II.exe",
                "exitCompanions": ["UnityCrashHandler64.exe"],

                "communityTools": {
                    "official": [
                        {
                            "name": "Brown Dust II Official",
                            "url": "https://www.browndust2.com/en-us/"
                        },
                        {
                            "name": "Brown Dust II News",
                            "url": "https://www.browndust2.com/en-us/news?page=0&type=all"
                        }
                    ],
                    "community": [
                        { "name": "Brown Dust 2 Database", "url": "https://dotgg.gg/brown-dust-2/" },
                        { "name": "Brown Dust 2 Tier Lists", "url": "https://dotgg.gg/brown-dust-2/tier-lists/" }
                    ]
                },

                "socialUrls": {
                    "discord": "https://discord.com/invite/qMbpbvWwja",
                    "youtube": "https://www.youtube.com/channel/UCmnj4VhKgycXSq3-GrQhhgQ"
                }
            }
        })
    })
}

// ------------ Profile Lookups ------------
// Small getters that read one field out of a profile, so callers do not poke at the raw JSON.
pub fn known_profile(id: &str) -> Option<&'static Value> {
    profiles().get(id)
}

pub fn profile(id: &str) -> &'static Value {
    match known_profile(id) {
        Some(p) => p,
        None => {
            log::warn!("game_profiles: unknown game id \"{id}\", falling back to WUWA.");
            profiles()
                .get(DEFAULT_GAME_ID)
                .expect("WUWA profile must exist")
        }
    }
}

pub fn list_profiles() -> Value {
    let list: Vec<Value> = GAME_IDS
        .iter()
        .map(|id| {
            let p = profile(id);
            json!({
                "id": p["id"],
                "displayName": p["displayName"],
                "shortName": p["shortName"],
                "supportsManagedInstall": is_managed(p),
            })
        })
        .collect();
    Value::Array(list)
}

pub fn is_managed(profile: &Value) -> bool {
    profile
        .get("supportsManagedInstall")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn install_mode(profile: &Value) -> Option<&str> {
    profile.get("installMode").and_then(Value::as_str)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    Default,
    Sophon,
    Netease,
    Hypergryph,
    Gf2,
    Bluepoch,
    Bd2,
    Unknown,
}

impl InstallMode {
    pub fn of(profile: &Value) -> Self {
        match install_mode(profile) {
            None => Self::Default,
            Some("sophon") => Self::Sophon,
            Some("netease") => Self::Netease,
            Some("hypergryph") => Self::Hypergryph,
            Some("gf2") => Self::Gf2,
            Some("bluepoch") => Self::Bluepoch,
            Some("bd2") => Self::Bd2,
            Some(_) => Self::Unknown,
        }
    }
}

pub fn display_name(profile: &Value) -> &str {
    profile["displayName"].as_str().unwrap_or("")
}

pub fn profile_id(profile: &Value) -> &str {
    profile["id"].as_str().unwrap_or(DEFAULT_GAME_ID)
}

pub fn executable_name(profile: &Value) -> &str {
    profile["executableName"].as_str().unwrap_or("")
}

pub fn install_folder_name(profile: &Value) -> String {
    let name = display_name(profile);
    let name = if name.is_empty() {
        profile_id(profile)
    } else {
        name
    };
    name.chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .collect::<String>()
        .trim()
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string()
}

pub const DEFAULT_MAX_PATH_TAIL: usize = 160;

pub fn max_path_tail(profile: &Value) -> usize {
    profile
        .get("maxPathTail")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_MAX_PATH_TAIL)
}

pub fn client_process_name(profile: &Value) -> &str {
    profile
        .get("clientProcessName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| executable_name(profile))
}

pub fn resource_quality_default(profile: &Value) -> Option<&str> {
    profile.get("resourceQualityArgs")?;
    profile.get("resourceQualityDefault").and_then(Value::as_str)
}

pub fn install_root_marker(profile: &Value) -> Option<Vec<String>> {
    string_array(profile.get("installRootMarker")?)
}

pub fn exit_companions(profile: &Value) -> Vec<String> {
    profile
        .get("exitCompanions")
        .and_then(string_array)
        .unwrap_or_default()
}

pub fn hidden_front_end(profile: &Value) -> Vec<String> {
    profile
        .get("hiddenFrontEnd")
        .and_then(string_array)
        .unwrap_or_default()
}

pub fn launches_hidden_front_end(profile: &Value, executable: &std::path::Path) -> bool {
    let Some(file_name) = executable.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    hidden_front_end(profile)
        .iter()
        .any(|name| name.eq_ignore_ascii_case(file_name))
}

pub struct LaunchCandidate {
    pub parts: Vec<String>,
    pub args: Vec<String>,
}

pub fn launch_candidates(profile: &Value) -> Vec<LaunchCandidate> {
    if let Some(list) = profile.get("launchCandidates").and_then(Value::as_array) {
        let candidates: Vec<LaunchCandidate> = list
            .iter()
            .filter_map(|entry| {
                Some(LaunchCandidate {
                    parts: string_array(entry.get("path")?)?,
                    args: entry.get("args").and_then(string_array).unwrap_or_default(),
                })
            })
            .collect();
        if !candidates.is_empty() {
            return candidates;
        }
    }

    vec![LaunchCandidate {
        parts: vec![executable_name(profile).to_string()],
        args: Vec::new(),
    }]
}

pub fn mod_config(profile: &Value) -> Option<&Value> {
    profile.get("mods").filter(|v| v.is_object())
}

pub fn mod_variant(profile: &Value) -> Option<&str> {
    mod_config(profile)?.get("variant")?.as_str()
}

pub fn mod_loader_mode(profile: &Value) -> &str {
    mod_config(profile)
        .and_then(|m| m.get("loaderMode"))
        .and_then(Value::as_str)
        .unwrap_or("hook")
}

pub fn mod_force_dx11_args(profile: &Value) -> Vec<String> {
    mod_config(profile)
        .and_then(|m| m.get("forceDx11Args"))
        .and_then(string_array)
        .unwrap_or_default()
}

pub fn mod_gamebanana_id(profile: &Value) -> Option<u64> {
    mod_config(profile)?.get("gameBananaId")?.as_u64()
}

pub fn mod_ini_defaults(profile: &Value) -> Vec<(String, String, String)> {
    let Some(Value::Object(sections)) = mod_config(profile).and_then(|m| m.get("modIniDefaults"))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (section, keys) in sections {
        let Value::Object(keys) = keys else {
            continue;
        };
        for (key, value) in keys {
            let text = match value {
                Value::String(s) => s.clone(),
                Value::Bool(b) => u8::from(*b).to_string(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            out.push((section.clone(), key.clone(), text));
        }
    }
    out
}

pub fn mod_is_experimental(profile: &Value) -> bool {
    mod_config(profile)
        .and_then(|m| m.get("experimental"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn string_array(v: &Value) -> Option<Vec<String>> {
    let arr = v.as_array()?;
    if arr.is_empty() {
        return None;
    }
    Some(
        arr.iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_names_a_known_install_mode() {
        for id in GAME_IDS {
            assert_ne!(InstallMode::of(profile(id)), InstallMode::Unknown, "{id}");
        }
        assert_eq!(InstallMode::of(&json!({})), InstallMode::Default);
        assert_eq!(InstallMode::of(&json!({ "installMode": "hoyoplay" })), InstallMode::Unknown);
    }

    #[test]
    fn managed_flag_reads_the_profile() {
        assert!(is_managed(profile("wuwa")));
        assert!(!is_managed(profile("gf1")));
        assert!(!is_managed(&json!({})));
        assert!(!is_managed(&json!({ "supportsManagedInstall": "true" })));
    }

    #[test]
    fn install_mode_is_optional_text() {
        assert_eq!(install_mode(profile("nte")), Some("netease"));
        assert_eq!(install_mode(profile("genshin")), Some("sophon"));
        assert_eq!(install_mode(&json!({})), None);
        assert_eq!(install_mode(&json!({ "installMode": 3 })), None);
    }

    #[test]
    fn list_profiles_reports_managed_state() {
        let list = list_profiles();
        let gf1 = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "gf1")
            .unwrap();
        assert_eq!(gf1["supportsManagedInstall"], Value::Bool(false));
    }

    #[test]
    fn only_the_nte_front_end_launches_hidden() {
        let nte = profile("nte");
        assert!(launches_hidden_front_end(
            nte,
            std::path::Path::new(r"D:\Games\NTE\NTEGlobal\NTEGlobalGame.exe")
        ));
        assert!(launches_hidden_front_end(
            nte,
            std::path::Path::new(r"D:\Games\NTE\NTEGlobal\ntegloBALgame.EXE")
        ));
        assert!(!launches_hidden_front_end(
            nte,
            std::path::Path::new(r"D:\Games\NTE\Client\WindowsNoEditor\HT\Binaries\Win64\HTGame.exe")
        ));
        assert!(!launches_hidden_front_end(
            profile("wuwa"),
            std::path::Path::new(r"D:\Games\NTEGlobalGame.exe")
        ));
        assert!(hidden_front_end(profile("hsr")).is_empty());
    }
}
