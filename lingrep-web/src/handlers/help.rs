use askama::Template;
use axum::{Router, response::Html, routing::get};

use crate::state::RequestStore;

pub struct HelpExample {
    pub title: &'static str,
    pub source: &'static str,
}

pub const HELP_EXAMPLES: &[HelpExample] = &[
    HelpExample {
        title: "Token constraints",
        source: "pattern terminal := [upos=VERB & unidic.cform=終止形].",
    },
    HelpExample {
        title: "Finite values",
        source: "pattern aru := [text in {ある, 有る}].",
    },
    HelpExample {
        title: "References",
        source: "fragment AUX := [upos=AUX].\npattern verb_aux := [upos=VERB] @AUX.",
    },
    HelpExample {
        title: "Alternation and quantifiers",
        source: "pattern sequence := ([upos=NOUN] | [upos=PRON])+ [upos=VERB].",
    },
    HelpExample {
        title: "Capture comparison",
        source: "pattern repeated := (?<unit> [upos=ADV]{1,2}) same_as($unit, by=unidic.kana).",
    },
    HelpExample {
        title: "Boundaries",
        source: "pattern whole_sentence := ^ [upos=VERB] $.",
    },
    HelpExample {
        title: "Regular lookaround",
        source: "pattern contextual := [text=そう] followed_by(([lemma=だ] $)).\npattern hearsay := not_preceded_by([unidic.cform=連用形-一般]) [text=そう].",
    },
    HelpExample {
        title: "Match modes",
        source: "pattern exact := [text=ない].\npattern one_of := [text in {ない, 無い}].\npattern substring := [text ~ ない].\npattern regex_search := [unidic.kana ~ /^タベ/].\npattern negated_search := [unidic.kana !~ /^タベ/].",
    },
];

#[derive(Template)]
#[template(path = "help.html")]
struct HelpPage {
    examples: &'static [HelpExample],
    current_page: &'static str,
}

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/help", get(help_page))
}

async fn help_page() -> Html<String> {
    HelpPage {
        examples: HELP_EXAMPLES,
        current_page: "help",
    }
    .render().map_or_else(|_| {
        Html(
            "<div class=\"alert alert-error\" role=\"status\">Unable to render page. Please retry.</div>"
                .to_string(),
        )
    }, Html)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};

    #[test]
    fn every_help_example_compiles_with_production_limits() {
        for example in HELP_EXAMPLES {
            compile_catalog(
                PatternSource::named("help.lx", example.source),
                CompileLimits::default(),
            )
            .unwrap_or_else(|error| panic!("{}: {error}", example.title));
        }
    }

    #[tokio::test]
    async fn help_describes_all_shipped_regular_lookaround_forms() {
        let Html(html) = help_page().await;

        for construct in [
            "preceded_by(E)",
            "not_preceded_by(E)",
            "followed_by(E)",
            "not_followed_by(E)",
        ] {
            assert!(
                html.contains(construct),
                "missing {construct} from lingrep patterns help"
            );
        }
        assert!(!html.contains("Arbitrary lookaround is not supported"));
    }

    #[tokio::test]
    async fn help_examples_tag_their_japanese_source_for_the_browser() {
        let Html(html) = help_page().await;

        assert!(
            html.contains("<pre lang=\"ja\"><code>"),
            "example source blocks should be tagged lang=\"ja\" (they contain Japanese)"
        );
    }
}
