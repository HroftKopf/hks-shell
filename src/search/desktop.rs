//! Installed-application provider: indexes `.desktop` files once at startup and
//! fuzzy-matches the query against application names.

use freedesktop_desktop_entry::{DesktopEntry, Iter, default_paths, get_languages_from_env};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use super::{Action, SearchProvider, SearchResult};

struct AppEntry {
    name: String,
    subtitle: Option<String>,
    icon: Option<String>,
    program: String,
    args: Vec<String>,
}

pub struct DesktopAppProvider {
    apps: Vec<AppEntry>,
    matcher: SkimMatcherV2,
}

impl DesktopAppProvider {
    /// Read and parse all `.desktop` files from the XDG data dirs once.
    pub fn load() -> Self {
        let locales = get_languages_from_env();
        let mut apps = Vec::new();

        for path in Iter::new(default_paths()) {
            let Ok(entry) = DesktopEntry::from_path(path, Some(locales.as_slice())) else {
                continue;
            };
            if entry.type_() != Some("Application") || entry.no_display() {
                continue;
            }
            let (Some(name), Some(exec)) = (entry.name(&locales), entry.exec()) else {
                continue;
            };

            // Exec may contain field codes (%U, %f, …); drop them, keep the rest.
            let mut tokens = exec.split_whitespace().filter(|t| !t.starts_with('%'));
            let Some(program) = tokens.next() else {
                continue;
            };

            let subtitle = entry
                .comment(&locales)
                .or_else(|| entry.generic_name(&locales))
                .map(|c| c.into_owned());

            apps.push(AppEntry {
                name: name.into_owned(),
                subtitle,
                icon: entry.icon().map(str::to_string),
                program: program.to_string(),
                args: tokens.map(str::to_string).collect(),
            });
        }

        Self {
            apps,
            matcher: SkimMatcherV2::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.apps.len()
    }
}

impl SearchProvider for DesktopAppProvider {
    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.apps
            .iter()
            .filter_map(|app| {
                self.matcher
                    .fuzzy_match(&app.name, query)
                    .map(|score| SearchResult {
                        title: app.name.clone(),
                        subtitle: app.subtitle.clone(),
                        icon: app.icon.clone(),
                        score,
                        action: Action::Launch {
                            program: app.program.clone(),
                            args: app.args.clone(),
                        },
                    })
            })
            .collect()
    }
}
