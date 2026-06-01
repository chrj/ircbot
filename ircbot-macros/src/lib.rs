//! Procedural macros for the [`ircbot`](https://docs.rs/ircbot) framework.
//!
//! These macros are re-exported by the `ircbot` crate — refer to its
//! documentation for usage.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    parse_macro_input, Expr, ExprLit, FnArg, Ident, ImplItem, ItemImpl, Lit, Meta, Pat, Type,
};

// ─── Custom parsers ──────────────────────────────────────────────────────────

/// Parses the `#[bot(...)]` attribute arguments.
///
/// Currently the only recognised argument is `state = <Type>`; an empty
/// attribute (`#[bot]`) yields `state: None`.
struct BotArgs {
    state: Option<Type>,
}

impl syn::parse::Parse for BotArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut state = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            let _: syn::Token![=] = input.parse()?;
            if key == "state" {
                state = Some(input.parse::<Type>()?);
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    format!("unknown #[bot] argument `{key}` (expected `state`)"),
                ));
            }
            if input.peek(syn::Token![,]) {
                let _: syn::Token![,] = input.parse()?;
            }
        }
        Ok(BotArgs { state })
    }
}

/// Parses `#[command("name")]` or `#[command("name", target = "...")]`
struct CommandArgs {
    name: String,
    target: Option<String>,
}

impl syn::parse::Parse for CommandArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let name: syn::LitStr = input.parse()?;
        let mut target = None;
        while input.peek(syn::Token![,]) {
            let _: syn::Token![,] = input.parse()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            let _: syn::Token![=] = input.parse()?;
            let val: syn::LitStr = input.parse()?;
            if key == "target" {
                target = Some(val.value());
            }
        }
        Ok(CommandArgs {
            name: name.value(),
            target,
        })
    }
}

// ─── #[bot] ──────────────────────────────────────────────────────────────────

/// Derive-like attribute that turns an `impl` block into a runnable IRC bot.
///
/// # Custom state
///
/// Pass `state = SomeType` to give the bot a public `state` field your handlers
/// can read:
///
/// ```ignore
/// #[derive(Default)]
/// struct Counter { hits: std::sync::atomic::AtomicUsize }
///
/// #[bot(state = Counter)]
/// impl MyBot {
///     #[command("ping")]
///     async fn ping(&self, ctx: ircbot::Context) -> ircbot::Result {
///         let n = self.state.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
///         ctx.reply(format!("pong #{n}"))
///     }
/// }
/// ```
///
/// The state type must implement [`Default`] (it is initialised with
/// `Default::default()` by both `MyBot::default()` and `MyBot::new`) and must be
/// `Send + Sync + 'static` (the bot is shared across tasks as an `Arc`; that
/// bound is checked at `main_loop`). Because handlers receive `&self`, mutating
/// state requires interior mutability — an `AtomicUsize`, a `Mutex<…>`, etc. To
/// start from a non-default value, assign the public field after constructing:
/// `let mut bot = MyBot::new(…).await?; bot.state = …;`.
///
/// Note: a `SIGHUP` hot-reload re-execs the binary, so in-memory `state` is
/// reconstructed via `Default` and is **not** carried across the reload.
///
/// This is sugar over the lower-level API: a bot is any
/// `Arc<T: Send + Sync + 'static>` passed to `ircbot::internal::run_bot` with a
/// hand-built `Vec<ircbot::HandlerEntry<T>>`, which you can use directly when you
/// want full control over the bot type.
///
/// # Panics
///
/// Panics at compile time if the annotated `impl` block does not use a simple
/// (non-generic, non-path) type name, e.g. `impl MyBot { … }`.
#[allow(clippy::too_many_lines)]
#[proc_macro_attribute]
pub fn bot(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as BotArgs);
    let input = parse_macro_input!(item as ItemImpl);

    let self_ty = &input.self_ty;
    let struct_name = match self_ty.as_ref() {
        Type::Path(tp) => tp
            .path
            .get_ident()
            .cloned()
            .expect("#[bot] expects a simple struct name"),
        _ => panic!("#[bot] expects a simple struct name"),
    };

    let mut handler_entries: Vec<TokenStream2> = Vec::new();
    let mut cleaned_methods: Vec<TokenStream2> = Vec::new();

    for item in &input.items {
        if let ImplItem::Fn(method) = item {
            let method_name = &method.sig.ident;

            // Extra args beyond &self and ctx
            let extra_args: Vec<(String, String)> = method
                .sig
                .inputs
                .iter()
                .skip(2)
                .filter_map(|arg| {
                    if let FnArg::Typed(pt) = arg {
                        let name = match pt.pat.as_ref() {
                            Pat::Ident(pi) => pi.ident.to_string(),
                            _ => "arg".to_string(),
                        };
                        let ty = match pt.ty.as_ref() {
                            Type::Path(tp) => tp
                                .path
                                .segments
                                .last()
                                .map(|s| s.ident.to_string())
                                .unwrap_or_default(),
                            _ => "Unknown".to_string(),
                        };
                        Some((name, ty))
                    } else {
                        None
                    }
                })
                .collect();

            let mut trigger_tokens: Option<TokenStream2> = None;
            let mut cleaned_attrs: Vec<syn::Attribute> = Vec::new();

            for attr in &method.attrs {
                let Some(ident) = attr.path().get_ident() else {
                    cleaned_attrs.push(attr.clone());
                    continue;
                };

                match ident.to_string().as_str() {
                    "command" => {
                        if let Meta::List(ml) = &attr.meta {
                            let args: CommandArgs =
                                syn::parse2(ml.tokens.clone()).unwrap_or(CommandArgs {
                                    name: String::new(),
                                    target: None,
                                });
                            let name = &args.name;
                            let target_ts = opt_str_ts(args.target.as_deref());
                            trigger_tokens = Some(quote! {
                                ircbot::Trigger::Command {
                                    name: #name.to_string(),
                                    target: #target_ts,
                                }
                            });
                        }
                    }
                    "on" => {
                        if let Meta::List(ml) = &attr.meta {
                            let metas_result = ml.parse_args_with(
                                syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                            );

                            let mut event: Option<String> = None;
                            let mut message: Option<String> = None;
                            let mut command_on: Option<String> = None;
                            let mut target: Option<String> = None;
                            let mut regex: Option<String> = None;
                            let mut mention = false;
                            let mut cron_interval: Option<String> = None;
                            let mut cron_tz: Option<String> = None;

                            if let Ok(metas) = metas_result {
                                for meta in metas {
                                    match &meta {
                                        Meta::Path(p) if p.is_ident("mention") => {
                                            mention = true;
                                        }
                                        Meta::NameValue(nv) => {
                                            let k = nv
                                                .path
                                                .get_ident()
                                                .map(ToString::to_string)
                                                .unwrap_or_default();
                                            if let Expr::Lit(ExprLit {
                                                lit: Lit::Str(s), ..
                                            }) = &nv.value
                                            {
                                                let v = s.value();
                                                match k.as_str() {
                                                    "event" => event = Some(v),
                                                    "message" => message = Some(v),
                                                    "command" => command_on = Some(v),
                                                    "target" => target = Some(v),
                                                    "regex" => regex = Some(v),
                                                    "cron" => cron_interval = Some(v),
                                                    "tz" => cron_tz = Some(v),
                                                    _ => {}
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            let target_ts = opt_str_ts(target.as_deref());
                            // Precedence: message > command > event > mention > cron.
                            // Only the first matching key wins; combining multiple
                            // trigger types in one `#[on(...)]` is not supported.
                            if let Some(msg_pat) = message {
                                trigger_tokens = Some(quote! {
                                    ircbot::Trigger::Message {
                                        pattern: #msg_pat.to_string(),
                                        target: #target_ts,
                                    }
                                });
                            } else if let Some(cmd) = command_on {
                                trigger_tokens = Some(quote! {
                                    ircbot::Trigger::Command {
                                        name: #cmd.to_string(),
                                        target: #target_ts,
                                    }
                                });
                            } else if let Some(ev) = event {
                                let regex_ts = opt_str_ts(regex.as_deref());
                                trigger_tokens = Some(quote! {
                                    ircbot::Trigger::Event {
                                        event: #ev.to_string(),
                                        target: #target_ts,
                                        regex: #regex_ts,
                                    }
                                });
                            } else if mention {
                                trigger_tokens = Some(quote! {
                                    ircbot::Trigger::Mention {
                                        target: #target_ts,
                                    }
                                });
                            } else if let Some(cron_str) = cron_interval {
                                // Validate the cron expression at compile time.
                                if let Err(e) = cron_str.parse::<cron::Schedule>() {
                                    panic!(
                                        "invalid cron expression {cron_str:?}: {e}\n\
                                         \n\
                                         The expression must use the 6-field Quartz format \
                                         with an optional 7th year field:\n\
                                         \n\
                                         sec  min  hour  day-of-month  month  day-of-week  [year]\n\
                                         \n\
                                         Examples:\n\
                                         \"0 0 * * * *\"          every hour (on the minute)\n\
                                         \"0 0 8-16 * * MON-FRI\" top of each hour, 8 a.m.–4 p.m., weekdays\n\
                                         \"0 */15 * * * *\"        every 15 minutes\n\
                                         \"0 0 9 * * MON\"         every Monday at 9 a.m."
                                    );
                                }
                                // Validate the timezone at compile time (defaults to UTC).
                                let tz_str = cron_tz.as_deref().unwrap_or("UTC");
                                if let Err(e) = tz_str.parse::<chrono_tz::Tz>() {
                                    panic!(
                                        "invalid timezone {tz_str:?}: {e}\n\
                                         \n\
                                         Use an IANA timezone name such as:\n\
                                         \"UTC\", \"America/New_York\", \"Europe/London\", \
                                         \"Asia/Tokyo\""
                                    );
                                }
                                let tz_str = tz_str.to_string();
                                trigger_tokens = Some(quote! {
                                    ircbot::Trigger::Cron {
                                        schedule: #cron_str.to_string(),
                                        tz: #tz_str.to_string(),
                                        target: #target_ts,
                                    }
                                });
                            }
                        }
                    }
                    _ => {
                        cleaned_attrs.push(attr.clone());
                    }
                }
            }

            if let Some(trigger) = trigger_tokens {
                let wrapper = build_wrapper(method_name, &extra_args);
                handler_entries.push(quote! {
                    ircbot::HandlerEntry {
                        trigger: #trigger,
                        handler: std::boxed::Box::new(#wrapper),
                    }
                });

                let mut cleaned = method.clone();
                cleaned.attrs = cleaned_attrs;
                cleaned_methods.push(quote! { #cleaned });
            } else {
                cleaned_methods.push(quote! { #method });
            }
        } else {
            let it = item;
            cleaned_methods.push(quote! { #it });
        }
    }

    // Optional user state field. When `state = Type` is absent both fragments are
    // empty, so the generated tokens are identical to the no-state case. The init
    // fragment carries a leading comma because the `__state` field in the struct
    // literals below has no trailing comma.
    let state_field_decl = match &args.state {
        Some(ty) => quote! { pub state: #ty, },
        None => quote! {},
    };
    let state_field_init = match &args.state {
        Some(_) => quote! { , state: std::default::Default::default() },
        None => quote! {},
    };

    quote! {
        pub struct #struct_name {
            __state: std::option::Option<ircbot::State>,
            #state_field_decl
        }

        impl Default for #struct_name {
            fn default() -> Self {
                #struct_name { __state: std::option::Option::None #state_field_init }
            }
        }

        impl #struct_name {
            /// Connect to an IRC server and return a bot ready to run.
            ///
            /// On Unix, if this process was started by `exec_reload` the live
            /// TCP connection is inherited from the parent binary and no new
            /// connection is made.  The `nick`, `server`, and `channels`
            /// arguments are used only when no inherited connection is present.
            pub async fn new(
                nick: impl Into<String>,
                server: impl AsRef<str>,
                channels: impl IntoIterator<Item = impl Into<String>>,
            ) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                // On Unix, check for an inherited fd from a hot-reload exec.
                #[cfg(unix)]
                if let Some(state) = ircbot::State::try_inherit_from_env()? {
                    eprintln!("[ircbot] hot-reload: resumed on inherited connection");
                    return Ok(#struct_name { __state: Some(state) #state_field_init });
                }

                let state = ircbot::State::connect(
                    nick.into(),
                    server.as_ref(),
                    channels.into_iter().map(|c| c.into()).collect(),
                ).await?;
                Ok(#struct_name { __state: Some(state) #state_field_init })
            }

            /// Run the bot's main event loop.
            ///
            /// On Unix, listens for `SIGHUP`.  When received, the current
            /// process execs the bot binary at the same path, passing the live
            /// TCP socket fd to the new process so the IRC connection is never
            /// interrupted.  If the exec fails the bot continues running.
            pub async fn main_loop(mut self) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
                let state = self.__state.take().expect("bot already started");

                #[cfg(unix)]
                let (raw_fd, reload_nick, reload_server, reload_channels,
                     reload_ka_interval_ms, reload_ka_timeout_ms) = (
                    state.raw_fd,
                    state.nick.clone(),
                    state.server.clone(),
                    state.channels.clone(),
                    state.keepalive_interval().as_millis() as u64,
                    state.keepalive_timeout().as_millis() as u64,
                );

                let bot_arc = std::sync::Arc::new(self);

                // Install a SIGHUP listener that execs the new binary with the
                // live fd inherited — zero-disconnect binary hot-reload.
                #[cfg(unix)]
                {
                    tokio::spawn(async move {
                        use tokio::signal::unix::{signal, SignalKind};
                        match signal(SignalKind::hangup()) {
                            Ok(mut stream) => {
                                while stream.recv().await.is_some() {
                                    eprintln!("[ircbot] SIGHUP — hot-reload: exec new binary");
                                    let err = ircbot::hot_reload::exec_reload(
                                        raw_fd,
                                        &reload_nick,
                                        &reload_server,
                                        &reload_channels,
                                        reload_ka_interval_ms,
                                        reload_ka_timeout_ms,
                                    );
                                    // exec_reload only returns on failure.
                                    eprintln!("[ircbot] hot-reload exec failed: {err}");
                                }
                            }
                            Err(e) => {
                                eprintln!("[ircbot] failed to install SIGHUP handler: {e}");
                            }
                        }
                    });
                }

                ircbot::internal::run_bot(bot_arc, state, #struct_name::__handlers()).await
            }

            fn __handlers() -> Vec<ircbot::HandlerEntry<#struct_name>> {
                vec![ #(#handler_entries),* ]
            }

            #(#cleaned_methods)*
        }
    }
    .into()
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn opt_str_ts(s: Option<&str>) -> TokenStream2 {
    if let Some(v) = s {
        quote! { Some(#v.to_string()) }
    } else {
        quote! { None }
    }
}

fn build_wrapper(method_name: &Ident, extra_args: &[(String, String)]) -> TokenStream2 {
    if extra_args.is_empty() {
        return quote! {
            |bot: std::sync::Arc<_>, ctx: ircbot::Context| -> ircbot::BoxFuture<ircbot::Result> {
                std::boxed::Box::pin(async move { bot.#method_name(ctx).await })
            }
        };
    }

    let mut extractions: Vec<TokenStream2> = Vec::new();
    let mut call_args: Vec<TokenStream2> = Vec::new();
    let mut str_idx = 0usize;

    for (name, ty) in extra_args {
        let ident = Ident::new(name, Span::call_site());
        call_args.push(quote! { #ident });
        match ty.as_str() {
            "User" => {
                extractions.push(quote! {
                    let #ident = ctx.sender.clone().unwrap_or_default();
                });
            }
            "String" => {
                let idx = str_idx;
                str_idx += 1;
                extractions.push(quote! {
                    let #ident: String = if !ctx.captures.is_empty() {
                        ctx.captures.get(#idx).cloned().unwrap_or_default()
                    } else {
                        ctx.message_text().to_string()
                    };
                });
            }
            _ => {
                let ty_ident = Ident::new(ty, Span::call_site());
                extractions.push(quote! {
                    let #ident: #ty_ident = Default::default();
                });
            }
        }
    }

    quote! {
        |bot: std::sync::Arc<_>, ctx: ircbot::Context| -> ircbot::BoxFuture<ircbot::Result> {
            std::boxed::Box::pin(async move {
                #(#extractions)*
                bot.#method_name(ctx, #(#call_args),*).await
            })
        }
    }
}

// ─── #[command] / #[on] as standalone no-ops ─────────────────────────────────

#[doc = include_str!("../docs/command.md")]
#[proc_macro_attribute]
pub fn command(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[doc = include_str!("../docs/on.md")]
#[proc_macro_attribute]
pub fn on(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
