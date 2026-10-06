//! Procedural macros for the [`ircbot`](https://docs.rs/ircbot) framework.
//!
//! These macros are re-exported by the `ircbot` crate — refer to its
//! documentation for usage.
#![warn(missing_docs)]

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    parse_macro_input, Expr, ExprLit, FnArg, Ident, ImplItem, ItemImpl, Lit, Meta, Pat, Type,
};

// ─── Custom parsers ──────────────────────────────────────────────────────────

/// Parses the `#[bot(...)]` attribute arguments.
///
/// The recognised arguments are `state = <Type>` and the `no_default` flag. An
/// empty attribute (`#[bot]`) yields `state: None`.
struct BotArgs {
    state: Option<Type>,
    no_default: bool,
}

impl syn::parse::Parse for BotArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut state = None;
        let mut no_default = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            if key == "no_default" {
                no_default = Some(key);
            } else if key == "state" {
                let _: syn::Token![=] = input.parse()?;
                state = Some(input.parse::<Type>()?);
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    format!("unknown #[bot] argument `{key}` (expected `state` or `no_default`)"),
                ));
            }
            if input.peek(syn::Token![,]) {
                let _: syn::Token![,] = input.parse()?;
            }
        }
        // Without a state type, the bot always has a `Default`, so the flag
        // has no effect. Refuse it instead of ignoring it.
        if let (Some(key), None) = (&no_default, &state) {
            return Err(syn::Error::new(
                key.span(),
                "`no_default` needs a state type: write `#[bot(state = MyState, no_default)]`",
            ));
        }
        Ok(BotArgs {
            state,
            no_default: no_default.is_some(),
        })
    }
}

/// Parses the `#[plugin(...)]` attribute arguments: `name = "..."` (required)
/// and `state = <Type>` (optional).
struct PluginArgs {
    name: syn::LitStr,
    state: Option<Type>,
}

impl syn::parse::Parse for PluginArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut name = None;
        let mut state = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            let _: syn::Token![=] = input.parse()?;
            if key == "name" {
                name = Some(input.parse::<syn::LitStr>()?);
            } else if key == "state" {
                state = Some(input.parse::<Type>()?);
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    format!("unknown #[plugin] argument `{key}` (expected `name` or `state`)"),
                ));
            }
            if input.peek(syn::Token![,]) {
                let _: syn::Token![,] = input.parse()?;
            }
        }
        let Some(name) = name else {
            return Err(syn::Error::new(
                Span::call_site(),
                "#[plugin] needs a name: write `#[plugin(name = \"my_plugin\")]`",
            ));
        };
        if !is_valid_plugin_name(&name.value()) {
            return Err(syn::Error::new(
                name.span(),
                format!(
                    "invalid plugin name {:?}: start with a lowercase ASCII letter, use \
                     only lowercase ASCII letters, digits and `_`, and do not use \
                     `sqlite` or a name that starts with `sqlite_`",
                    name.value()
                ),
            ));
        }
        Ok(PluginArgs { name, state })
    }
}

/// Whether `name` obeys the rules for a plugin name.
///
/// The rules are the same as for a store namespace in `ircbot`, because a
/// plugin uses its name as its namespace. Keep this function the same as
/// `is_valid_name` in `ircbot/src/name.rs`.
fn is_valid_plugin_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let reserved = name == "sqlite" || name.starts_with("sqlite_");
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !reserved
}

/// Parses `#[command("name")]`, `#[command("name", target = "...")]`,
/// `#[command("name", role = "...")]`, and/or the `include_self` and `raw`
/// flags.
struct CommandArgs {
    name: String,
    target: Option<String>,
    role: Option<String>,
    scope: Option<String>,
    include_self: bool,
    raw_text: bool,
}

impl syn::parse::Parse for CommandArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let name: syn::LitStr = input.parse()?;
        let mut target = None;
        let mut role = None;
        let mut scope = None;
        let mut include_self = false;
        let mut raw_text = false;
        while input.peek(syn::Token![,]) {
            let _: syn::Token![,] = input.parse()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            // `include_self` and `raw` are flags; every other key takes a
            // string value.
            if key == "include_self" {
                include_self = true;
                continue;
            }
            if key == "raw" {
                raw_text = true;
                continue;
            }
            let _: syn::Token![=] = input.parse()?;
            let val: syn::LitStr = input.parse()?;
            if key == "target" {
                target = Some(val.value());
            } else if key == "role" {
                role = Some(val.value());
            } else if key == "scope" {
                scope = Some(val.value());
            }
        }
        Ok(CommandArgs {
            name: name.value(),
            target,
            role,
            scope,
            include_self,
            raw_text,
        })
    }
}

// ─── #[bot] ──────────────────────────────────────────────────────────────────

/// Derive-like attribute that turns an `impl` block into a runnable IRC bot.
///
/// The macro also implements `ircbot::Bot` for the type. This trait gives the
/// list of handlers, and `ircbot::testing::TestBot` uses it in tests.
///
/// # Starting the bot
///
/// `MyBot::new(nick, server, channels)` makes the bot. It does not connect.
/// The `with_*` methods set roles, the ignore list, keepalive, flood control,
/// reconnect delays, CTCP `VERSION` and keepnick. `main_loop` checks the
/// setup, connects, and runs:
///
/// ```ignore
/// MyBot::new("mybot", "irc.example.net:6667", ["rust"])
///     .with_role("admin", ["*!*@trusted.host"])
///     .main_loop()
///     .await?;
/// ```
///
/// `main_loop` refuses to start when a command needs a role that no
/// `with_role` call defines, because such a command would never run.
///
/// # Plugins
///
/// The generated `plugin` method adds a `#[plugin]` to the bot. The bot runs
/// its own handlers in the dispatch loop, and each plugin in its own task. See
/// the `ircbot::plugin` module for the checks and the isolation of plugins.
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
/// The state type must be `Send + Sync + 'static` (the bot is shared across
/// tasks as an `Arc`; that bound is checked at `main_loop`). Unless you add the
/// `no_default` flag (see below), it must also implement [`Default`]: both
/// `MyBot::default()` and `MyBot::new` initialise it with `Default::default()`.
/// Because handlers receive `&self`, mutating state requires interior
/// mutability — an `AtomicUsize`, a `Mutex<…>`, etc. To start from a
/// non-default value, use `MyBot::new_with_state(…, state)`. It
/// takes the state as a fourth argument and does not call `Default::default()`.
///
/// # State without `Default`
///
/// Some state has no useful default value, for example a handle to an open
/// database. Add the `no_default` flag for this state:
///
/// ```ignore
/// struct Data { db: MyDatabase }
///
/// #[bot(state = Data, no_default)]
/// impl MyBot { /* … */ }
///
/// let data = Data { db: MyDatabase::open("bot.db")? };
/// MyBot::new_with_state("mybot", "irc.example.net:6667", ["rust"], data)
///     .main_loop()
///     .await?;
/// ```
///
/// With `no_default`, the macro does not generate `impl Default for MyBot` or
/// `MyBot::new`. Build the bot with `MyBot::new_with_state`, or with
/// `MyBot::from_state` in a test. The flag needs `state = SomeType`. Without a
/// state type, the macro refuses it.
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

    let (handler_entries, cleaned_methods) = expand_handlers(&input);

    // Optional user state field. When `state = Type` is absent both fragments are
    // empty, so the generated tokens are the same as without state. The init
    // fragment has a leading comma, because the `__setup` field in the struct
    // literals below has no trailing comma.
    let state_field_decl = match &args.state {
        Some(ty) => quote! { pub state: #ty, },
        None => quote! {},
    };
    let state_field_init = match &args.state {
        Some(_) => quote! { , state: std::default::Default::default() },
        None => quote! {},
    };
    // The constructors that take a state the caller built. The macro emits
    // them only when the bot has a `state` field.
    let state_constructors = match &args.state {
        Some(ty) => quote! {
            /// Make a bot that connects as `nick` to `server` and joins
            /// `channels`, with a pre-built `state`. This does not connect:
            /// [`main_loop`](Self::main_loop) does.
            ///
            /// Use this when the state needs work or input that `Default`
            /// cannot give, for example a database path or a config value.
            /// This constructor does not call `Default::default()`.
            ///
            /// ```rust,ignore
            /// let state = MyState::open("bot.db")?;
            /// MyBot::new_with_state("mybot", "irc.example.net:6667", ["rust"], state)
            ///     .main_loop()
            ///     .await?;
            /// ```
            #[must_use]
            pub fn new_with_state(
                nick: impl Into<String>,
                server: impl Into<ircbot::Server>,
                channels: impl IntoIterator<Item = impl Into<String>>,
                state: #ty,
            ) -> Self {
                #struct_name {
                    __setup: ircbot::internal::BotSetup::new(nick, server, channels),
                    state,
                }
            }

            /// Make the bot from a pre-built `state`, with no server.
            ///
            /// This is the intended way to unit-test handlers. Handlers take
            /// `&self` and reach the connection only when they send a reply,
            /// which in tests is captured by a
            /// [`TestContext`](ircbot::testing::TestContext) instead. A bot
            /// made this way cannot run: its `main_loop` returns
            /// [`StartError::NoServer`](ircbot::StartError::NoServer).
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
            #[must_use]
            pub fn from_state(state: #ty) -> Self {
                #struct_name {
                    __setup: std::default::Default::default(),
                    state,
                }
            }
        },
        None => quote! {},
    };

    // `Default` and `new` both build the state with `Default::default()`. With
    // `no_default`, the state type has no `Default`, so the macro leaves out
    // both.
    let default_impl = if args.no_default {
        quote! {}
    } else {
        quote! {
            impl Default for #struct_name {
                fn default() -> Self {
                    #struct_name { __setup: std::default::Default::default() #state_field_init }
                }
            }
        }
    };
    let new_method = if args.no_default {
        quote! {}
    } else {
        quote! {
            /// Make a bot that connects as `nick` to `server` and joins
            /// `channels`. This does not connect:
            /// [`main_loop`](Self::main_loop) does.
            ///
            /// `server` is anything that converts into an
            /// [`ircbot::Server`](ircbot::Server). A bare `"host:port"` string
            /// connects in plaintext; with the `tls` feature, `Server::tls`
            /// connects over TLS:
            ///
            /// ```rust,ignore
            /// MyBot::new("mybot", "irc.example.net:6667", ["rust"]);
            /// MyBot::new("mybot", Server::tls("irc.libera.chat:6697"), ["rust"]);
            /// ```
            #[must_use]
            pub fn new(
                nick: impl Into<String>,
                server: impl Into<ircbot::Server>,
                channels: impl IntoIterator<Item = impl Into<String>>,
            ) -> Self {
                #struct_name {
                    __setup: ircbot::internal::BotSetup::new(nick, server, channels)
                    #state_field_init
                }
            }
        }
    };

    quote! {
        pub struct #struct_name {
            __setup: ircbot::internal::BotSetup,
            #state_field_decl
        }

        #default_impl

        impl #struct_name {
            #new_method

            #state_constructors

            /// Add `plugin`. Each plugin runs in its own task. See the
            /// [`plugin` module](ircbot::plugin) for the checks and the
            /// isolation of plugins.
            #[must_use]
            pub fn plugin<P: ircbot::Plugin>(mut self, plugin: P) -> Self {
                self.__setup.add_plugin(plugin);
                self
            }

            /// Set how many messages can wait in the queue of each plugin. The
            /// default is [`DEFAULT_PLUGIN_QUEUE_CAPACITY`](ircbot::DEFAULT_PLUGIN_QUEUE_CAPACITY).
            /// A value of 0 is changed to 1, and a value above the largest queue
            /// that Tokio permits is changed to that size.
            #[must_use]
            pub fn with_queue_capacity(mut self, capacity: usize) -> Self {
                self.__setup.set_queue_capacity(capacity);
                self
            }

            /// Define an access-control role named `name`, authorising any
            /// sender whose `nick!user@host` matches one of the given hostmask
            /// glob patterns (`*` wildcard). Commands annotated with
            /// `#[command(..., role = "name")]` only fire for matching
            /// senders; everyone else is silently ignored.
            ///
            /// `main_loop` refuses to start when a command needs a role that
            /// no call defines. See [`State::with_role`](ircbot::State::with_role).
            #[must_use]
            pub fn with_role(
                mut self,
                name: impl Into<String>,
                masks: impl IntoIterator<Item = impl Into<String>>,
            ) -> Self {
                self.__setup.add_role(name, masks);
                self
            }

            /// Ignore the senders whose hostmask matches one of `masks`. See
            /// [`State::with_ignore`](ircbot::State::with_ignore).
            #[must_use]
            pub fn with_ignore(
                mut self,
                masks: impl IntoIterator<Item = impl Into<String>>,
            ) -> Self {
                self.__setup.add_ignore(masks);
                self
            }

            /// Set the keepalive interval and timeout. See
            /// [`State::with_keepalive`](ircbot::State::with_keepalive).
            #[must_use]
            pub fn with_keepalive(
                mut self,
                interval: std::time::Duration,
                timeout: std::time::Duration,
            ) -> Self {
                self.__setup.set_keepalive(interval, timeout);
                self
            }

            /// Set the flood control. See
            /// [`State::with_flood_control`](ircbot::State::with_flood_control).
            #[must_use]
            pub fn with_flood_control(
                mut self,
                burst: usize,
                rate: std::time::Duration,
            ) -> Self {
                self.__setup.set_flood_control(burst, rate);
                self
            }

            /// Override the reconnect delays. After a lost connection the bot
            /// waits `delay`, then attempts to reconnect. Each failed attempt
            /// doubles the delay, up to `max_delay`, and the bot retries until
            /// it is connected again. The delay returns to `delay` after a
            /// connection that reached registration. The defaults are 5 seconds
            /// and 5 minutes.
            #[must_use]
            pub fn with_reconnect(
                mut self,
                delay: std::time::Duration,
                max_delay: std::time::Duration,
            ) -> Self {
                self.__setup.set_reconnect(delay, max_delay);
                self
            }

            /// Set a custom CTCP `VERSION` reply.
            ///
            /// By default the bot answers CTCP `VERSION` with
            /// `ircbot <crate-version>`. Call this to reply with your own
            /// identifier instead.
            #[must_use]
            pub fn with_ctcp_version(mut self, version: impl Into<String>) -> Self {
                self.__setup.set_ctcp_version(version);
                self
            }

            /// Enable keepnick: periodically re-attempt to reclaim the
            /// originally-requested nick whenever the bot is using a different
            /// one. Disabled by default.
            #[must_use]
            pub fn with_keepnick_interval(mut self, interval: std::time::Duration) -> Self {
                self.__setup.set_keepnick_interval(interval);
                self
            }

            /// Enable keepnick with the default reclaim interval
            /// ([`DEFAULT_KEEPNICK_INTERVAL`](ircbot::DEFAULT_KEEPNICK_INTERVAL)).
            #[must_use]
            pub fn with_keepnick(mut self) -> Self {
                self.__setup.set_keepnick_interval(ircbot::DEFAULT_KEEPNICK_INTERVAL);
                self
            }

            /// Check the setup, connect, and run the bot with its plugins.
            ///
            /// Before it connects, this checks the commands of the bot and of
            /// its plugins: see the [`plugin` module](ircbot::plugin). The bot
            /// reconnects on its own when the connection is lost, and retries
            /// until it is connected again, so this does not return while the
            /// process runs.
            ///
            /// # Errors
            ///
            /// Returns a [`StartError`](ircbot::StartError) (in the
            /// `BoxError`) if a check fails, if the bot has no server, or if
            /// the first connection fails.
            pub async fn main_loop(mut self) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
                let setup = std::mem::take(&mut self.__setup);
                setup.run(self).await
            }

            #(#cleaned_methods)*
        }

        impl ircbot::Bot for #struct_name {
            fn handlers() -> std::vec::Vec<ircbot::HandlerEntry<#struct_name>> {
                std::vec![ #(#handler_entries),* ]
            }
        }
    }
    .into()
}

// ─── #[plugin] ───────────────────────────────────────────────────────────────

/// Turns an `impl` block into a plugin that a `#[bot]` can run.
///
/// A plugin has handlers, as a `#[bot]` has, but no connection of its own. Add
/// it to a bot with the generated `plugin` method. Each plugin runs in its own
/// task, so a slow or failing plugin does not stop the bot or the others. See
/// the `ircbot::plugin` module.
///
/// ```ignore
/// #[plugin(name = "greeter")]
/// impl Greeter {
///     #[command("hello")]
///     async fn hello(&self, ctx: ircbot::Context) -> ircbot::Result {
///         ctx.reply("hello!")
///     }
/// }
/// ```
///
/// # Arguments
///
/// * `name = "..."` (required): the name of the plugin. It starts with a
///   lowercase ASCII letter, and has only lowercase ASCII letters, digits and
///   `_`. It cannot be `sqlite` or start with `sqlite_`. A bot refuses to
///   start with two plugins of the same name. A plugin that keeps data uses this name as
///   its store namespace.
/// * `state = Type` (optional): gives the plugin a public `state` field, as
///   for `#[bot]`. Make the plugin with `MyPlugin::from_state(state)`. The
///   state type does not need `Default`.
///
/// Without `state`, the plugin is a unit struct: make it with
/// `MyPlugin::default()` or `MyPlugin`.
///
/// The macro implements `ircbot::Plugin` and `ircbot::Bot` for the type. Thus
/// `ircbot::testing::TestBot` can send a test line through the handlers of a
/// plugin.
///
/// # Panics
///
/// Panics at compile time if the annotated `impl` block does not use a simple
/// (non-generic, non-path) type name, e.g. `impl MyPlugin { … }`.
#[proc_macro_attribute]
pub fn plugin(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as PluginArgs);
    let input = parse_macro_input!(item as ItemImpl);

    let struct_name = match input.self_ty.as_ref() {
        Type::Path(tp) => tp
            .path
            .get_ident()
            .cloned()
            .expect("#[plugin] expects a simple struct name"),
        _ => panic!("#[plugin] expects a simple struct name"),
    };
    let name = &args.name;
    let (handler_entries, cleaned_methods) = expand_handlers(&input);

    let definition = match &args.state {
        Some(ty) => quote! {
            pub struct #struct_name {
                /// The state of the plugin.
                pub state: #ty,
            }

            impl #struct_name {
                /// Make the plugin with `state`.
                #[must_use]
                pub fn from_state(state: #ty) -> Self {
                    #struct_name { state }
                }
            }
        },
        None => quote! {
            #[derive(Default)]
            pub struct #struct_name;
        },
    };

    quote! {
        #definition

        impl #struct_name {
            #(#cleaned_methods)*
        }

        impl ircbot::Bot for #struct_name {
            fn handlers() -> std::vec::Vec<ircbot::HandlerEntry<#struct_name>> {
                std::vec![ #(#handler_entries),* ]
            }
        }

        impl ircbot::Plugin for #struct_name {
            const NAME: &'static str = #name;
        }
    }
    .into()
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Build the handler list and the cleaned methods of an `impl` block.
///
/// Each method with a `#[command]` or `#[on(...)]` attribute gives one
/// `ircbot::HandlerEntry` expression. The methods come back without these
/// attributes. `#[bot]` and `#[plugin]` share this, so both accept the same
/// handlers.
#[allow(clippy::too_many_lines)]
fn expand_handlers(input: &ItemImpl) -> (Vec<TokenStream2>, Vec<TokenStream2>) {
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
            // Whether the handler also gets the messages of the bot itself.
            let mut include_self = false;
            // Whether the trigger matches the text with the formatting codes.
            let mut raw_text = false;
            // Which kind of target the handler answers.
            let mut scope: Option<String> = None;
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
                                    scope: None,
                                    include_self: false,
                                    raw_text: false,
                                });
                            let name = &args.name;
                            command_name = Some(args.name.clone());
                            include_self = args.include_self;
                            raw_text = args.raw_text;
                            scope = args.scope.clone();
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
                            let mut action: Option<String> = None;
                            let mut ctcp: Option<String> = None;
                            let mut cron_interval: Option<String> = None;
                            let mut cron_tz: Option<String> = None;
                            let mut role: Option<String> = None;
                            let mut scope_on: Option<String> = None;

                            if let Ok(metas) = metas_result {
                                for meta in metas {
                                    match &meta {
                                        Meta::Path(p) if p.is_ident("mention") => {
                                            mention = true;
                                        }
                                        Meta::Path(p) if p.is_ident("include_self") => {
                                            include_self = true;
                                        }
                                        Meta::Path(p) if p.is_ident("raw") => {
                                            raw_text = true;
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
                                                    "action" => action = Some(v),
                                                    "ctcp" => ctcp = Some(v),
                                                    "cron" => cron_interval = Some(v),
                                                    "tz" => cron_tz = Some(v),
                                                    "role" => role = Some(v),
                                                    "scope" => scope_on = Some(v),
                                                    _ => {}
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            if scope_on.is_some() {
                                scope = scope_on;
                            }

                            let target_ts = opt_str_ts(target.as_deref());
                            let role_ts = opt_str_ts(role.as_deref());
                            // Precedence: message > command > event > mention > action
                            // > ctcp > cron.
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
                            } else if let Some(action_pat) = action {
                                trigger_tokens = Some(quote! {
                                    ircbot::Trigger::Action {
                                        pattern: #action_pat.to_string(),
                                        target: #target_ts,
                                    }
                                });
                            } else if let Some(ctcp_cmd) = ctcp {
                                validate_ctcp_command(&ctcp_cmd);
                                trigger_tokens = Some(quote! {
                                    ircbot::Trigger::Ctcp {
                                        command: #ctcp_cmd.to_string(),
                                        target: #target_ts,
                                    }
                                });
                            } else if let Some(cron_str) = cron_interval {
                                if scope.is_some() {
                                    panic!(
                                        "`scope` has no meaning with `cron`\n\
                                         \n\
                                         A cron handler fires on a schedule, not on a\n\
                                         message, so it answers no channel or query.\n\
                                         Use `target` to name where it sends."
                                    );
                                }
                                if raw_text {
                                    panic!(
                                        "`raw` has no meaning with `cron`\n\
                                         \n\
                                         A cron handler fires on a schedule, not on a\n\
                                         message, so it has no text. Remove `raw` from\n\
                                         this handler."
                                    );
                                }
                                if include_self {
                                    panic!(
                                        "`include_self` has no meaning with `cron`\n\
                                         \n\
                                         A cron handler fires on a schedule, not on a\n\
                                         message, so it has no sender. Remove\n\
                                         `include_self` from this handler."
                                    );
                                }
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
                let scope_ts = scope_tokens(scope.as_deref());
                let wrapper = build_wrapper(method_name, &extra_args, command_name.as_deref());
                handler_entries.push(quote! {
                    ircbot::HandlerEntry {
                        trigger: #trigger,
                        include_self: #include_self,
                        raw_text: #raw_text,
                        scope: #scope_ts,
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

    (handler_entries, cleaned_methods)
}

/// The `ircbot::Scope` value for the `scope` option.
///
/// # Panics
///
/// Panics at compile time when `scope` is neither `"channel"` nor `"private"`.
fn scope_tokens(scope: Option<&str>) -> TokenStream2 {
    match scope {
        None => quote! { ircbot::Scope::Any },
        Some("channel") => quote! { ircbot::Scope::Channel },
        Some("private") => quote! { ircbot::Scope::Private },
        Some(other) => panic!(
            "invalid scope {other:?}\n\
             \n\
             Use \"channel\" for a handler that answers only in a channel, or\n\
             \"private\" for one that answers only a private message.\n\
             Leave `scope` out for a handler that answers both."
        ),
    }
}

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

/// Reject a CTCP command that can never fire, at compile time.
///
/// # Panics
///
/// Panics when `command` is not a single word, or when it names a command that
/// the framework answers itself (`PING`, `VERSION`).
fn validate_ctcp_command(command: &str) {
    if command.is_empty() || command.chars().any(|c| c.is_whitespace() || c == '\x01') {
        panic!(
            "invalid CTCP command {command:?}\n\
             \n\
             Use a single word, without spaces or \\x01 bytes, such as:\n\
             \"TIME\", \"CLIENTINFO\", \"DCC\"\n\
             \n\
             To match the text of a /me action, use `action = \"pattern\"`."
        );
    }
    if command.eq_ignore_ascii_case("PING") || command.eq_ignore_ascii_case("VERSION") {
        panic!(
            "a handler for CTCP {command:?} never fires\n\
             \n\
             The framework answers CTCP PING and VERSION itself.\n\
             To change the VERSION reply, use `State::with_ctcp_version`."
        );
    }
}

/// Argument extraction for non-command triggers (message/event/mention/action/ctcp).
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
