//! Search engine: a set of providers that turn a query string into ranked
//! results. Kept provider-generic so later sources (commands, files,
//! calculator, …) plug in without touching the launcher UI.

mod desktop;

pub use desktop::DesktopAppProvider;

/// A single result row.
#[derive(Clone)]
pub struct SearchResult {
    pub title: String,
    pub subtitle: Option<String>,
    /// Freedesktop icon name (or absolute path) for the row icon.
    pub icon: Option<String>,
    /// Higher = better match.
    pub score: i64,
    pub action: Action,
}

/// What happens when a result is activated (Enter).
#[derive(Clone)]
pub enum Action {
    /// Spawn a program detached from the shell.
    Launch { program: String, args: Vec<String> },
}

impl Action {
    pub fn run(&self) {
        match self {
            Action::Launch { program, args } => {
                // Detached: the launched app outlives the launcher process.
                if let Err(err) = std::process::Command::new(program).args(args).spawn() {
                    eprintln!("failed to launch {program}: {err}");
                }
            }
        }
    }
}

/// A source of results for a query.
pub trait SearchProvider {
    fn search(&self, query: &str) -> Vec<SearchResult>;
}

/// Aggregates providers and ranks their combined results.
pub struct Search {
    providers: Vec<Box<dyn SearchProvider>>,
}

impl Search {
    pub fn new() -> Self {
        Self {
            providers: vec![Box::new(DesktopAppProvider::load())],
        }
    }

    /// Query all providers and return the best results, highest score first.
    pub fn query(&self, query: &str) -> Vec<SearchResult> {
        if query.is_empty() {
            return Vec::new();
        }
        let mut results: Vec<SearchResult> = self
            .providers
            .iter()
            .flat_map(|provider| provider.search(query))
            .collect();
        results.sort_by(|a, b| b.score.cmp(&a.score));
        results.truncate(20);
        results
    }
}
