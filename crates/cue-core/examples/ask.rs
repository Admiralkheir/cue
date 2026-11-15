//! Asks a real model a question with a stored key, the way the overlay does, and prints the
//! streamed answer, each web search and the usage. For trying effort and search settings
//! against live APIs; every run is billed to the key.
//!
//! cargo run -p cue-core --example ask -- [options] "question"
//!
//!   --account <id|kind>    account from the config, by id or kind (anthropic, open_code_zen…);
//!                          the first account by default
//!   --model <id>           the model to ask; the fast model's id by default
//!   --effort <level>       default, low, medium or high (default: default)
//!   --search <mode>        off, provider or searxng[=url] (default: off)
//!   --image <file.jpg>     a screenshot to ask about

use std::ops::ControlFlow;
use std::time::Instant;
use std::{env, fs};

use cue_core::capture::EncodedFrame;
use cue_core::providers::{self, Event, Options, Search};
use cue_core::search::{DEFAULT_SEARXNG_URL, SearxNg};
use cue_core::{Config, Conversation, Effort, Role, secrets};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut account = None;
    let mut model = None;
    let mut effort = Effort::Default;
    let mut search = "off".to_owned();
    let mut image = None;
    let mut question = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--account" => account = Some(value()?),
            "--model" => model = Some(value()?),
            "--effort" => {
                effort = match value()?.as_str() {
                    "default" => Effort::Default,
                    "low" => Effort::Low,
                    "medium" => Effort::Medium,
                    "high" => Effort::High,
                    other => return Err(format!("unknown effort {other}").into()),
                }
            }
            "--search" => search = value()?,
            "--image" => image = Some(fs::read(value()?)?),
            _ => question = Some(arg),
        }
    }
    let question = question.ok_or("pass a question")?;

    let config = Config::load_from(&Config::default_path()?)?;
    let account = match &account {
        Some(wanted) => config
            .accounts
            .iter()
            .find(|account| &account.id == wanted || account.kind.slug() == wanted),
        None => config.accounts.first(),
    }
    .ok_or("no matching account in the config")?;
    let model = model
        .or_else(|| config.fast_model.as_ref().map(|model| model.model.clone()))
        .ok_or("pass --model")?;

    let searxng = search
        .strip_prefix("searxng")
        .map(|url| SearxNg::new(url.strip_prefix('=').unwrap_or(DEFAULT_SEARXNG_URL)));
    let options = Options {
        effort,
        search: match (&searxng, search.as_str()) {
            (Some(searxng), _) => Search::Tool(searxng),
            (None, "provider") => Search::Provider,
            (None, "off") => Search::Off,
            (None, other) => return Err(format!("unknown search mode {other}").into()),
        },
    };

    let mut conversation = Conversation {
        id: cue_core::unique_id("conversation"),
        system: "You are Cue, an assistant for someone playing a video game right now. Lead \
                 with the answer and keep it to a few short sentences."
            .into(),
        image: image.map(|jpeg| EncodedFrame {
            width: 0,
            height: 0,
            jpeg,
        }),
        turns: Vec::new(),
    };
    conversation.push(Role::User, question);

    let key = secrets::api_key(&account.id)?;
    let client = providers::connect(account, key)?;
    eprintln!(
        "asking {model} via {} (effort {effort:?}, search {search})",
        account.display_name()
    );
    let started = Instant::now();
    let mut first_text = None;
    let completion = client.stream(&model, options, &conversation, &mut |event| {
        match event {
            Event::Text(piece) => {
                first_text.get_or_insert_with(|| started.elapsed());
                print!("{piece}");
            }
            Event::Searching(query) => eprintln!("\n[searching: {}]", query.unwrap_or("?")),
        }
        ControlFlow::Continue(())
    })?;
    println!();
    eprintln!(
        "{:?} · {} in / {} out tokens · {} web searches · first text after {:.1?} · done after {:.1?}",
        completion.stop,
        completion.usage.input_tokens,
        completion.usage.output_tokens,
        completion.usage.web_searches,
        first_text.unwrap_or_default(),
        started.elapsed()
    );
    Ok(())
}
