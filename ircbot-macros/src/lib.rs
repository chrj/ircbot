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

/// Parses `#[command("name")]`, `#[command("name", target = "...")]`, and/or
/// `#[command("name", role = "...")]`.
struct CommandArgs {
    name: String,
    target: Option<String>,
    role: Option<String>,
}

impl syn::parse::Parse for CommandArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let name: syn::LitStr = input.parse()?;
        let mut target = None;
        let mut role = None;
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
            } else if key == "role" {
                role = Some(val.value());
            }
        }
        Ok(CommandArgs {
            name: name.value(),
            target,
            role,
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

            // Extra args beyond &self and ctx, retaining the full parsed type so
            // command handlers can parse typed positional arguments.
            let extra_args: Vec<(Ident, Type)> = method
                .sig
                .inputs
                .iter()
                .skip(2)
                .filter_map(|arg| {
                    if let FnArg::Typed(pt) = arg {
                        let name = match pt.pat.as_ref() {
                            Pat::Ident(pi) => pi.ident.clone(),
                            _ => Ident::new("arg", Span::call_site()),
                        };
                        Some((name, (*pt.ty).clone()))
                    } else {
                        None
                    }
                })
                .collect();

            let mut trigger_tokens: Option<TokenStream2> = None;
            // The command keyword, if this handler is triggered by a command
            // (via `#[command]` or `#[on(command = "...")]`). Drives typed
            // argument parsing and the generated usage string.
            let mut command_name: Option<String> = None;
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
                                    role: None,
                                });
                            let name = &args.name;
                            command_name = Some(args.name.clone());
                            let target_ts = opt_str_ts(args.target.as_deref());
                            let role_ts = opt_str_ts(args.role.as_deref());
                            trigger_tokens = Some(quote! {
                                ircbot::Trigger::Command {
                                    name: #name.to_string(),
                                    target: #target_ts,
                                    role: #role_ts,
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
                            let mut role: Option<String> = None;

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
                                                    "role" => role = Some(v),
                                                    _ => {}
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            let target_ts = opt_str_ts(target.as_deref());
                            let role_ts = opt_str_ts(role.as_deref());
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
                                command_name = Some(cmd.clone());
                                trigger_tokens = Some(quote! {
                                    ircbot::Trigger::Command {
                                        name: #cmd.to_string(),
                                        target: #target_ts,
                                        role: #role_ts,
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
                let wrapper = build_wrapper(method_name, &extra_args, command_name.as_deref());
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
    // A constructor that takes a pre-built state and attaches no live
    // connection. Only meaningful when the bot has a `state` field, so it is
    // emitted solely in the `state = Type` case. This is the supported entry
    // point for unit-testing handlers (see `ircbot::testing`): it bypasses the
    // `Default` impl, which would build state via `Default::default()` — wrong
    // for any state that opens files, sockets, or other real resources.
    let from_state_method = match &args.state {
        Some(ty) => quote! {
            /// Construct the bot from a pre-built `state`, with no live IRC
            /// connection attached.
            ///
            /// This is the intended way to unit-test handlers. Handlers take
            /// `&self` and reach the connection only when they send a reply,
            /// which in tests is captured by a
            /// [`TestContext`](ircbot::testing::TestContext) instead — so a bot
            /// built this way can drive handlers directly without ever touching
            /// the network.
            ///
            /// Prefer this over [`Default::default`] whenever your state type's
            /// `Default` does real work (opening a database, reading config,
            /// connecting to a service): `from_state` lets the test build a
            /// purpose-made state — an in-memory store, a temp-dir fixture —
            /// and inject it directly.
            ///
            /// # Example
            ///
            /// ```rust,no_run
            /// # use ircbot::{bot, Context, Result};
            /// # use ircbot::testing::TestContext;
            /// #[derive(Default)]
            /// struct State { greeting: String }
            ///
            /// #[bot(state = State)]
            /// impl Greeter {
            ///     #[on(mention)]
            ///     async fn hello(&self, ctx: Context, _text: String) -> Result {
            ///         ctx.reply(self.state.greeting.clone())
            ///     }
            /// }
            ///
            /// #[tokio::test]
            /// async fn replies_with_configured_greeting() {
            ///     let bot = Greeter::from_state(State { greeting: "hi!".into() });
            ///     let mut tc = TestContext::channel("#test", "alice", "greeter: yo");
            ///     bot.hello(tc.take_ctx(), "yo".into()).await.unwrap();
            ///     // `reply` prefixes the sender's nick in a channel.
            ///     assert_eq!(tc.next_reply().as_deref(), Some("PRIVMSG #test :alice, hi!\r\n"));
            /// }
            /// ```
            pub fn from_state(state: #ty) -> Self {
                #struct_name { __state: std::option::Option::None, state }
            }
        },
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
                    channels.into_iter().map(|c| ircbot::Channel::from(c.into())).collect(),
                ).await?;
                Ok(#struct_name { __state: Some(state) #state_field_init })
            }

            #from_state_method

            /// Set a custom CTCP `VERSION` reply.
            ///
            /// By default the bot answers CTCP `VERSION` with
            /// `ircbot <crate-version>`. Call this (before `main_loop`) to reply
            /// with your own identifier instead. The value is re-applied on a
            /// `SIGHUP` hot-reload, since the builder runs again on startup.
            #[must_use]
            pub fn with_ctcp_version(mut self, version: impl Into<String>) -> Self {
                if let Some(state) = self.__state.take() {
                    self.__state = Some(state.with_ctcp_version(version));
                }
                self
            }

            /// Enable keepnick: periodically re-attempt to reclaim the
            /// originally-requested nick whenever the bot is using a different
            /// one. Disabled by default. Call this (before `main_loop`); the
            /// value is re-applied on a `SIGHUP` hot-reload, since the builder
            /// runs again on startup.
            #[must_use]
            pub fn with_keepnick_interval(mut self, interval: std::time::Duration) -> Self {
                if let Some(state) = self.__state.take() {
                    self.__state = Some(state.with_keepnick_interval(interval));
                }
                self
            }

            /// Enable keepnick with the default reclaim interval
            /// (60 seconds). Convenience wrapper around
            /// `with_keepnick_interval`.
            #[must_use]
            pub fn with_keepnick(mut self) -> Self {
                if let Some(state) = self.__state.take() {
                    self.__state = Some(state.with_keepnick());
                }
                self
            }

            /// Define an access-control role named `name`, authorising any
            /// sender whose `nick!user@host` matches one of the given hostmask
            /// glob patterns (`*` wildcard). Commands annotated with
            /// `#[command(..., role = #name)]` only fire for matching senders;
            /// everyone else is silently ignored.
            ///
            /// Call this (before `main_loop`); like the other builders it is
            /// re-applied on a `SIGHUP` hot-reload, since the builder runs again
            /// on startup. May be called repeatedly to add patterns or roles.
            #[must_use]
            pub fn with_role(
                mut self,
                name: impl Into<String>,
                masks: impl IntoIterator<Item = impl Into<String>>,
            ) -> Self {
                if let Some(state) = self.__state.take() {
                    self.__state = Some(state.with_role(name, masks));
                }
                self
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
                    state.nick.as_str().to_string(),
                    state.server.clone(),
                    state.channels.iter().map(|c| c.as_str().to_string()).collect::<std::vec::Vec<String>>(),
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

/// How a handler parameter's declared type is sourced from a message.
enum TypeClass {
    /// The message sender (`User`).
    User,
    /// A `String`.
    StringTy,
    /// `Option<Inner>`; `is_string` is true for `Option<String>`.
    Opt { inner: Type, is_string: bool },
    /// `Vec<Inner>`; `is_string` is true for `Vec<String>`.
    VecTy { inner: Type, is_string: bool },
    /// Any other type, parsed from a single token via `FromStr`.
    Scalar(Type),
}

/// The last path segment of a `Type::Path` (e.g. `Option` of `std::option::Option`).
fn type_last_seg(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(tp) => tp.path.segments.last(),
        _ => None,
    }
}

/// Whether `ty`'s final path segment is the identifier `name`.
fn type_is(ty: &Type, name: &str) -> bool {
    type_last_seg(ty).is_some_and(|s| s.ident == name)
}

/// The first generic type argument of `ty` (e.g. `i64` of `Option<i64>`).
fn generic_inner(ty: &Type) -> Option<Type> {
    let seg = type_last_seg(ty)?;
    if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
        for arg in &ab.args {
            if let syn::GenericArgument::Type(t) = arg {
                return Some(t.clone());
            }
        }
    }
    None
}

/// Classify a parameter type for argument extraction.
fn classify(ty: &Type) -> TypeClass {
    if type_is(ty, "User") {
        return TypeClass::User;
    }
    if type_is(ty, "String") {
        return TypeClass::StringTy;
    }
    if type_is(ty, "Option") {
        if let Some(inner) = generic_inner(ty) {
            let is_string = type_is(&inner, "String");
            return TypeClass::Opt { inner, is_string };
        }
    }
    if type_is(ty, "Vec") {
        if let Some(inner) = generic_inner(ty) {
            let is_string = type_is(&inner, "String");
            return TypeClass::VecTy { inner, is_string };
        }
    }
    TypeClass::Scalar(ty.clone())
}

fn build_wrapper(
    method_name: &Ident,
    extra_args: &[(Ident, Type)],
    command_name: Option<&str>,
) -> TokenStream2 {
    if extra_args.is_empty() {
        return quote! {
            |bot: std::sync::Arc<_>, ctx: ircbot::Context| -> ircbot::BoxFuture<ircbot::Result> {
                std::boxed::Box::pin(async move { bot.#method_name(ctx).await })
            }
        };
    }

    let call_args: Vec<TokenStream2> = extra_args
        .iter()
        .map(|(name, _)| quote! { #name })
        .collect();

    let extractions: Vec<TokenStream2> = if let Some(cmd) = command_name {
        command_extractions(extra_args, cmd)
    } else {
        legacy_extractions(extra_args)
    };

    quote! {
        |bot: std::sync::Arc<_>, ctx: ircbot::Context| -> ircbot::BoxFuture<ircbot::Result> {
            std::boxed::Box::pin(async move {
                #(#extractions)*
                bot.#method_name(ctx, #(#call_args),*).await
            })
        }
    }
}

/// Argument extraction for non-command triggers (message/event/mention).
///
/// Preserves historical behaviour: each `String` parameter maps to the trigger
/// capture group at its positional index, `User` becomes the sender, and any
/// other type is filled with `Default::default()`.
fn legacy_extractions(extra_args: &[(Ident, Type)]) -> Vec<TokenStream2> {
    let mut out = Vec::new();
    let mut str_idx = 0usize;
    for (name, ty) in extra_args {
        match classify(ty) {
            TypeClass::User => out.push(quote! {
                let #name = ctx.sender.clone().unwrap_or_default();
            }),
            TypeClass::StringTy => {
                let idx = str_idx;
                str_idx += 1;
                out.push(quote! {
                    let #name: String = if !ctx.captures.is_empty() {
                        ctx.captures.get(#idx).cloned().unwrap_or_default()
                    } else {
                        ctx.message_text().to_string()
                    };
                });
            }
            _ => out.push(quote! {
                let #name: #ty = std::default::Default::default();
            }),
        }
    }
    out
}

/// Argument extraction for command triggers: typed positional parsing of the
/// command tail, replying with a generated usage string (and skipping the
/// handler) when a required argument is missing or fails to parse.
fn command_extractions(extra_args: &[(Ident, Type)], cmd: &str) -> Vec<TokenStream2> {
    // The last argument sourced from the tail (everything except `User`); a
    // trailing `String` here captures the rest of the line.
    let last_tail_idx = extra_args.iter().rposition(|(_, ty)| !type_is(ty, "User"));

    // Build the usage string from the signature.
    let mut usage_parts: Vec<String> = Vec::new();
    for (name, ty) in extra_args {
        match classify(ty) {
            TypeClass::User => {}
            TypeClass::Opt { .. } => usage_parts.push(format!("[{name}]")),
            TypeClass::VecTy { .. } => usage_parts.push(format!("[{name}...]")),
            _ => usage_parts.push(format!("<{name}>")),
        }
    }
    let usage = if usage_parts.is_empty() {
        format!("usage: !{cmd}")
    } else {
        format!("usage: !{cmd} {}", usage_parts.join(" "))
    };
    let usage_fail = quote! {
        { let _ = ctx.reply(#usage); return std::result::Result::Ok(()); }
    };

    // `next_token` needs `&mut __args`; the rest-consuming helpers take `self`.
    // Only declare `__args` mutable when a token is actually pulled, to avoid an
    // `unused_mut` warning under `-D warnings`.
    let needs_mut = extra_args.iter().enumerate().any(|(i, (_, ty))| {
        let is_last_tail = Some(i) == last_tail_idx;
        match classify(ty) {
            TypeClass::User => false,
            TypeClass::StringTy => !is_last_tail,
            TypeClass::Scalar(_) => true,
            TypeClass::Opt { is_string, .. } => !is_string,
            TypeClass::VecTy { .. } => false,
        }
    });
    let has_tail_args = extra_args.iter().any(|(_, ty)| !type_is(ty, "User"));

    let mut out: Vec<TokenStream2> = Vec::new();
    if has_tail_args {
        let binding = if needs_mut {
            quote! { let mut __args = ircbot::internal::Args::new(&__tail); }
        } else {
            quote! { let __args = ircbot::internal::Args::new(&__tail); }
        };
        out.push(quote! {
            let __tail: String = ctx.captures.first().cloned().unwrap_or_default();
            #binding
        });
    }

    for (i, (name, ty)) in extra_args.iter().enumerate() {
        let is_last_tail = Some(i) == last_tail_idx;
        match classify(ty) {
            TypeClass::User => out.push(quote! {
                let #name = ctx.sender.clone().unwrap_or_default();
            }),
            TypeClass::StringTy if is_last_tail => out.push(quote! {
                let #name: String = __args.rest().to_string();
            }),
            TypeClass::StringTy => out.push(quote! {
                let #name: String = match __args.next_token() {
                    Some(t) => t.to_string(),
                    None => #usage_fail,
                };
            }),
            TypeClass::Scalar(scalar) => out.push(quote! {
                let #name: #scalar = match __args.next_token() {
                    Some(t) => match t.parse::<#scalar>() {
                        Ok(v) => v,
                        Err(_) => #usage_fail,
                    },
                    None => #usage_fail,
                };
            }),
            TypeClass::Opt {
                is_string: true, ..
            } => out.push(quote! {
                let #name: Option<String> = {
                    let __r = __args.rest();
                    if __r.is_empty() { None } else { Some(__r.to_string()) }
                };
            }),
            TypeClass::Opt { inner, .. } => out.push(quote! {
                let #name: Option<#inner> = match __args.next_token() {
                    Some(t) => match t.parse::<#inner>() {
                        Ok(v) => Some(v),
                        Err(_) => #usage_fail,
                    },
                    None => None,
                };
            }),
            TypeClass::VecTy {
                is_string: true, ..
            } => out.push(quote! {
                let #name: Vec<String> = __args.rest_tokens();
            }),
            TypeClass::VecTy { inner, .. } => out.push(quote! {
                let #name: Vec<#inner> = {
                    let mut __out: Vec<#inner> = std::vec::Vec::new();
                    for __t in __args.rest_tokens() {
                        match __t.parse::<#inner>() {
                            Ok(v) => __out.push(v),
                            Err(_) => #usage_fail,
                        }
                    }
                    __out
                };
            }),
        }
    }
    out
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
