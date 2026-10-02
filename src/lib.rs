//! essentials-pumpkin — a Pumpkin port of the most-used EssentialsX features.
//!
//! Build with `cargo build --release` (target `wasm32-wasip2` is set in
//! `.cargo/config.toml`) and drop the resulting `.wasm` into the server's
//! `plugins/` folder.

mod economy;
mod homes;
mod player;
mod social;
mod state;
mod teleport;
mod util;

use std::sync::Arc;

use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata, Result, Server,
    command::{ArgumentType, Command, CommandNode, StringType},
    events::{
        EventData, EventHandler, EventPriority, PlayerChatEvent, PlayerJoinEvent, PlayerLeaveEvent,
        PlayerTeleportEvent,
    },
    permission::{Permission, PermissionDefault, PermissionLevel},
    permissions::{FS_READ_DATA, FS_WRITE_DATA},
    register_plugin,
};

use economy::EcoOp;
use state::{Loc, now, with};
use util::*;

struct EssentialsPlugin;

impl Plugin for EssentialsPlugin {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "essentials-pumpkin".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Port of EssentialsX concepts for Pumpkin".into()],
            description: "Homes, warps, /back, TPA, messaging, economy and admin utilities.".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<()> {
        let folder = context.get_data_folder();
        with(|st| {
            st.folder = folder;
            st.load();
        });

        let context = Arc::new(context);

        // Register commands immediately on load
        register_commands(&context);
        tracing::info!("essentials-pumpkin commands registered");

        context.register_event_handler::<PlayerJoinEvent, _>(
            JoinHandler,
            EventPriority::Normal,
            false,
        )?;
        context.register_event_handler::<PlayerLeaveEvent, _>(
            LeaveHandler,
            EventPriority::Normal,
            false,
        )?;
        context.register_event_handler::<PlayerTeleportEvent, _>(
            BackTracker,
            EventPriority::Lowest,
            false,
        )?;
        // Blocking + high priority so a muted player's message can be cancelled.
        context.register_event_handler::<PlayerChatEvent, _>(
            MuteFilter,
            EventPriority::High,
            true,
        )?;

        tracing::info!("essentials-pumpkin loaded");
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<()> {
        with(|st| st.save());
        Ok(())
    }
}

register_plugin!(EssentialsPlugin);

// ------------------------------------------------------------------ events

struct JoinHandler;
impl EventHandler<PlayerJoinEvent> for JoinHandler {
    fn handle(&self, server: Server, ev: EventData<PlayerJoinEvent>) -> EventData<PlayerJoinEvent> {
        let (id, name) = (id_of(&ev.player), ev.player.get_name());
        let (nick, prefix, motd, show_motd, vanished_ids) = with(|st| {
            let d = st.pd(&id, &name);
            let nick = d.nick.clone();
            st.save();
            let vanished: Vec<String> = st
                .data
                .players
                .iter()
                .filter(|(_, d)| d.vanished)
                .map(|(i, _)| i.clone())
                .collect();
            (
                nick,
                st.config.nick_prefix.clone(),
                st.config.motd.clone(),
                st.config.show_motd_on_join,
                vanished,
            )
        });
        if let Some(n) = nick {
            ev.player.set_display_name(text(&format!("{prefix}{n}")));
        }
        for vid in vanished_ids {
            if vid != id {
                if let Some(v) = lookup(&server, &vid) {
                    ev.player.hide_player(v);
                }
            }
        }
        if show_motd {
            for line in motd {
                tell_fmt(&ev.player, &line.replace("{player}", &name));
            }
        }
        ev
    }
}

struct LeaveHandler;
impl EventHandler<PlayerLeaveEvent> for LeaveHandler {
    fn handle(&self, _s: Server, ev: EventData<PlayerLeaveEvent>) -> EventData<PlayerLeaveEvent> {
        let (id, name) = (id_of(&ev.player), ev.player.get_name());
        with(|st| {
            st.pd(&id, &name).last_seen = now();
            st.afk.remove(&id);
            st.tpa.remove(&id);
            st.tpa.retain(|target, req| target != &id && req.from != id);
            st.save();
        });
        ev
    }
}

/// Remembers where a player teleported *from*, for /back.
struct BackTracker;
impl EventHandler<PlayerTeleportEvent> for BackTracker {
    fn handle(
        &self,
        _s: Server,
        ev: EventData<PlayerTeleportEvent>,
    ) -> EventData<PlayerTeleportEvent> {
        if !ev.cancelled {
            let (x, y, z) = ev.from_position;
            let loc = Loc {
                world: ev.player.get_world().get_name(),
                x,
                y,
                z,
                yaw: ev.player.get_yaw(),
                pitch: ev.player.get_pitch(),
            };
            let id = id_of(&ev.player);
            with(|st| {
                st.back.insert(id, loc);
            });
        }
        ev
    }
}

struct MuteFilter;
impl EventHandler<PlayerChatEvent> for MuteFilter {
    fn handle(&self, _s: Server, mut ev: EventData<PlayerChatEvent>) -> EventData<PlayerChatEvent> {
        let id = id_of(&ev.player);
        if with(|st| st.data.players.get(&id).is_some_and(|d| d.muted)) {
            ev.cancelled = true;
            tell(&ev.player, "You are muted and cannot chat.");
        }
        ev
    }
}

// --------------------------------------------------------------- commands

/// Register a permission node and then the command that requires it.
fn reg(
    ctx: &Context,
    key: &str,
    names: &[&str],
    desc: &str,
    admin: bool,
    build: impl FnOnce(Command) -> Command,
) {
    let node = format!("essentials-pumpkin:{key}");
    let default = if admin {
        PermissionDefault::Op(PermissionLevel::Two)
    } else {
        PermissionDefault::Allow
    };
    if let Err(e) = ctx.register_permission(&Permission {
        node: node.clone(),
        description: desc.to_string(),
        default,
        children: vec![],
    }) {
        tracing::warn!("could not register permission {node}: {e}");
    }
    let names: Vec<String> = names.iter().map(|s| (*s).to_string()).collect();
    ctx.register_command(build(Command::new(&names, desc)), &node);
}

// Pumpkin's Wasm host does not implement ArgumentType::Message yet.
fn greedy_text_node(name: &str) -> CommandNode {
    CommandNode::argument(name, &ArgumentType::String(StringType::Greedy))
}

fn register_commands(ctx: &Context) {
    // ---- homes -----------------------------------------------------------
    reg(
        ctx,
        "sethome",
        &["sethome", "esethome", "createhome", "ecreatehome"],
        "Set a home at your location.",
        false,
        |c| {
            c.execute(h(|s, _, _| homes::set_home(s, homes::default_home_name())))
                .then(word_node("name").execute(h(|s, _, a| {
                    homes::set_home(s, &arg_str(a, "name").unwrap_or_default())
                })))
        },
    );
    reg(
        ctx,
        "home",
        &["home", "ehome", "homes", "ehomes"],
        "Teleport to a home.",
        false,
        |c| {
            c.execute(h(|s, sv, _| homes::home_default(s, sv)))
                .then(word_node("name").execute(h(|s, sv, a| {
                    homes::go_home(s, sv, &arg_str(a, "name").unwrap_or_default())
                })))
        },
    );
    reg(
        ctx,
        "delhome",
        &[
            "delhome", "edelhome", "remhome", "eremhome", "rmhome", "ermhome",
        ],
        "Delete a home.",
        false,
        |c| {
            c.then(word_node("name").execute(h(|s, _, a| {
                homes::del_home(s, &arg_str(a, "name").unwrap_or_default())
            })))
        },
    );

    // ---- warps -----------------------------------------------------------
    reg(
        ctx,
        "setwarp",
        &["setwarp", "esetwarp", "createwarp", "ecreatewarp"],
        "Create a warp at your location.",
        true,
        |c| {
            c.then(word_node("warp").execute(h(|s, _, a| {
                homes::set_warp(s, &arg_str(a, "warp").unwrap_or_default())
            })))
        },
    );
    reg(
        ctx,
        "warp",
        &["warp", "ewarp", "warps", "ewarps"],
        "Teleport to a warp, or list warps.",
        false,
        |c| {
            c.execute(h(|s, _, _| homes::list_warps(s)))
                .then(word_node("warp").execute(h(|s, sv, a| {
                    homes::go_warp(s, sv, &arg_str(a, "warp").unwrap_or_default())
                })))
        },
    );
    reg(
        ctx,
        "delwarp",
        &[
            "delwarp", "edelwarp", "remwarp", "eremwarp", "rmwarp", "ermwarp",
        ],
        "Delete a warp.",
        true,
        |c| {
            c.then(word_node("warp").execute(h(|s, _, a| {
                homes::del_warp(s, &arg_str(a, "warp").unwrap_or_default())
            })))
        },
    );

    // ---- teleportation ---------------------------------------------------
    reg(
        ctx,
        "back",
        &["back", "eback", "return", "ereturn"],
        "Return to your previous location.",
        false,
        |c| c.execute(h(|s, sv, _| teleport::back(s, sv))),
    );
    reg(
        ctx,
        "top",
        &["top", "etop"],
        "Teleport to the highest block above you.",
        false,
        |c| c.execute(h(|s, _, _| teleport::top(s))),
    );
    // Vanilla already owns /tp, so only Essentials' alternative names are registered.
    reg(
        ctx,
        "tp",
        &["etp", "tele", "etele", "eteleport", "tp2p", "etp2p"],
        "Teleport to a player (or one player to another).",
        true,
        |c| {
            c.then(
                players_node("player")
                    .execute(h(|s, sv, a| teleport::tp(s, sv, a, false)))
                    .then(
                        players_node("other").execute(h(|s, sv, a| teleport::tp(s, sv, a, true))),
                    ),
            )
        },
    );
    reg(
        ctx,
        "tphere",
        &["tphere", "etphere", "s"],
        "Teleport a player to you.",
        true,
        |c| c.then(players_node("player").execute(h(|s, sv, a| teleport::tphere(s, sv, a)))),
    );
    reg(
        ctx,
        "tpall",
        &["tpall", "etpall"],
        "Teleport every player to you.",
        true,
        |c| c.execute(h(|s, sv, _| teleport::tpall(s, sv))),
    );
    reg(
        ctx,
        "tppos",
        &["tppos", "etppos"],
        "Teleport to coordinates.",
        true,
        |c| {
            c.then(num_node("x").then(
                num_node("y").then(num_node("z").execute(h(|s, _, a| teleport::tppos(s, a)))),
            ))
        },
    );
    reg(
        ctx,
        "tpa",
        &["tpa", "etpa", "call", "ecall", "tpask", "etpask"],
        "Ask to teleport to a player.",
        false,
        |c| c.then(players_node("player").execute(h(|s, _, a| teleport::tpa_request(s, a, false)))),
    );
    reg(
        ctx,
        "tpahere",
        &["tpahere", "etpahere"],
        "Ask a player to teleport to you.",
        false,
        |c| c.then(players_node("player").execute(h(|s, _, a| teleport::tpa_request(s, a, true)))),
    );
    reg(
        ctx,
        "tpaccept",
        &["tpaccept", "etpaccept", "tpyes", "etpyes"],
        "Accept a teleport request.",
        false,
        |c| c.execute(h(|s, sv, _| teleport::tpaccept(s, sv))),
    );
    reg(
        ctx,
        "tpdeny",
        &["tpdeny", "etpdeny", "tpno", "etpno"],
        "Deny a teleport request.",
        false,
        |c| c.execute(h(|s, sv, _| teleport::tpdeny(s, sv))),
    );
    reg(
        ctx,
        "tpacancel",
        &["tpacancel", "etpacancel"],
        "Cancel your outgoing teleport request.",
        false,
        |c| c.execute(h(|s, _, _| teleport::tpacancel(s))),
    );

    // ---- chat & info -----------------------------------------------------
    // Vanilla owns /msg, /tell, /w and /me, so they are exposed under Essentials' other aliases.
    reg(
        ctx,
        "emsg",
        &["m", "t", "pm", "epm", "etell", "whisper", "ewhisper"],
        "Send a private message.",
        false,
        |c| {
            c.then(
                players_node("target")
                    .then(greedy_text_node("message").execute(h(|s, _, a| social::msg(s, a)))),
            )
        },
    );
    reg(
        ctx,
        "r",
        &["r", "er", "reply", "ereply"],
        "Reply to the last message.",
        false,
        |c| c.then(greedy_text_node("message").execute(h(|s, sv, a| social::reply(s, sv, a)))),
    );
    reg(
        ctx,
        "eme",
        &["action", "eaction", "describe", "edescribe"],
        "Describe an action in chat.",
        false,
        |c| c.then(greedy_text_node("action").execute(h(|s, sv, a| social::action(s, sv, a)))),
    );
    reg(
        ctx,
        "broadcast",
        &[
            "broadcast",
            "bc",
            "ebc",
            "bcast",
            "ebcast",
            "ebroadcast",
            "shout",
            "eshout",
        ],
        "Broadcast a message to everyone.",
        true,
        |c| c.then(greedy_text_node("message").execute(h(|_, sv, a| social::announce(sv, a)))),
    );
    reg(
        ctx,
        "nick",
        &["nick", "enick", "nickname", "enickname"],
        "Change your (or another player's) nickname.",
        false,
        |c| {
            c.then(word_node("nick").execute(h(|s, _, a| social::nick(s, a, None))))
                .then(players_node("player").then(
                    word_node("nick").execute(h(|s, _, a| social::nick(s, a, Some("player")))),
                ))
        },
    );
    reg(
        ctx,
        "mute",
        &["mute", "emute", "silence", "esilence", "unmute", "eunmute"],
        "Toggle a player's chat mute.",
        true,
        |c| c.then(players_node("player").execute(h(|s, _, a| social::mute(s, a)))),
    );
    reg(
        ctx,
        "motd",
        &["motd", "emotd"],
        "Show the message of the day.",
        false,
        |c| c.execute(h(|s, _, _| social::motd(s))),
    );
    reg(
        ctx,
        "rules",
        &["rules", "erules"],
        "Show the server rules.",
        false,
        |c| c.execute(h(|s, _, _| social::rules(s))),
    );
    reg(
        ctx,
        "elist",
        &[
            "online",
            "eonline",
            "playerlist",
            "eplayerlist",
            "plist",
            "eplist",
            "who",
            "ewho",
        ],
        "List online players.",
        false,
        |c| c.execute(h(|s, sv, _| social::list(s, sv))),
    );
    reg(
        ctx,
        "getpos",
        &[
            "getpos",
            "egetpos",
            "coords",
            "ecoords",
            "whereami",
            "ewhereami",
            "getloc",
            "egetloc",
        ],
        "Show your (or another player's) coordinates.",
        false,
        |c| {
            c.execute(h(|s, _, a| social::getpos(s, a, None))).then(
                players_node("player").execute(h(|s, _, a| social::getpos(s, a, Some("player")))),
            )
        },
    );
    reg(
        ctx,
        "ping",
        &["ping", "eping", "pong", "epong", "echo", "eecho"],
        "Show your latency.",
        false,
        |c| c.execute(h(|s, _, _| social::ping(s))),
    );
    reg(
        ctx,
        "near",
        &["near", "enear", "nearby", "enearby"],
        "List nearby players.",
        false,
        |c| {
            c.execute(h(|s, sv, a| social::near(s, sv, a, false)))
                .then(num_node("radius").execute(h(|s, sv, a| social::near(s, sv, a, true))))
        },
    );
    reg(
        ctx,
        "seen",
        &["seen", "eseen"],
        "When was a player last online?",
        false,
        |c| c.then(word_node("name").execute(h(|s, sv, a| social::seen(s, sv, a)))),
    );
    reg(
        ctx,
        "afk",
        &["afk", "eafk", "away", "eaway"],
        "Toggle your AFK status.",
        false,
        |c| c.execute(h(|s, sv, _| social::afk(s, sv))),
    );

    // ---- player state ----------------------------------------------------
    reg(
        ctx,
        "heal",
        &["heal", "eheal"],
        "Restore health and hunger.",
        true,
        |c| {
            c.execute(h(|s, _, a| player::heal(s, a, None))).then(
                players_node("player").execute(h(|s, _, a| player::heal(s, a, Some("player")))),
            )
        },
    );
    reg(
        ctx,
        "feed",
        &["feed", "efeed", "eat", "eeat"],
        "Restore hunger.",
        true,
        |c| {
            c.execute(h(|s, _, a| player::feed(s, a, None))).then(
                players_node("player").execute(h(|s, _, a| player::feed(s, a, Some("player")))),
            )
        },
    );
    reg(ctx, "fly", &["fly", "efly"], "Toggle flight.", true, |c| {
        c.execute(h(|s, _, a| player::fly(s, a, None)))
            .then(players_node("player").execute(h(|s, _, a| player::fly(s, a, Some("player")))))
    });
    reg(
        ctx,
        "god",
        &["god", "egod", "godmode", "egodmode", "tgm", "etgm"],
        "Toggle god mode.",
        true,
        |c| {
            c.execute(h(|s, _, a| player::god(s, a, None))).then(
                players_node("player").execute(h(|s, _, a| player::god(s, a, Some("player")))),
            )
        },
    );
    reg(
        ctx,
        "speed",
        &[
            "speed",
            "espeed",
            "flyspeed",
            "eflyspeed",
            "walkspeed",
            "ewalkspeed",
        ],
        "Change walking/flying speed (1-10).",
        true,
        |c| {
            c.then(num_node("speed").execute(h(|s, _, a| player::speed(s, a, None))))
                .then(
                    CommandNode::literal("fly").then(
                        num_node("speed").execute(h(|s, _, a| player::speed(s, a, Some(true)))),
                    ),
                )
                .then(
                    CommandNode::literal("walk").then(
                        num_node("speed").execute(h(|s, _, a| player::speed(s, a, Some(false)))),
                    ),
                )
        },
    );
    reg(
        ctx,
        "vanish",
        &["vanish", "v", "ev", "evanish"],
        "Hide yourself from other players.",
        true,
        |c| c.execute(h(|s, sv, _| player::vanish(s, sv))),
    );
    reg(
        ctx,
        "suicide",
        &["suicide", "esuicide"],
        "Take your own life.",
        false,
        |c| c.execute(h(|s, sv, _| player::suicide(s, sv))),
    );
    reg(
        ctx,
        "enderchest",
        &[
            "enderchest",
            "eenderchest",
            "echest",
            "eechest",
            "ec",
            "eec",
        ],
        "Open your ender chest.",
        false,
        |c| c.execute(h(|s, _, _| player::enderchest(s))),
    );
    reg(
        ctx,
        "ptime",
        &["ptime", "eptime", "playertime", "eplayertime"],
        "Set your personal time.",
        false,
        |c| c.then(word_node("time").execute(h(|s, _, a| player::ptime(s, a)))),
    );
    reg(
        ctx,
        "pweather",
        &["pweather", "epweather", "playerweather", "eplayerweather"],
        "Set your personal weather.",
        false,
        |c| c.then(word_node("weather").execute(h(|s, _, a| player::pweather(s, a)))),
    );

    // ---- economy ---------------------------------------------------------
    reg(
        ctx,
        "balance",
        &["balance", "bal", "ebal", "ebalance", "money", "emoney"],
        "Show your (or another player's) balance.",
        false,
        |c| {
            c.execute(h(|s, _, a| economy::balance(s, a, false)))
                .then(word_node("name").execute(h(|s, _, a| economy::balance(s, a, true))))
        },
    );
    reg(
        ctx,
        "pay",
        &["pay", "epay"],
        "Pay another player.",
        false,
        |c| {
            c.then(
                players_node("player")
                    .then(num_node("amount").execute(h(|s, _, a| economy::pay(s, a)))),
            )
        },
    );
    reg(
        ctx,
        "balancetop",
        &["balancetop", "ebalancetop", "baltop", "ebaltop"],
        "Show the richest players.",
        false,
        |c| c.execute(h(|s, _, _| economy::baltop(s))),
    );
    reg(
        ctx,
        "eco",
        &["eco", "eeco", "economy", "eeconomy"],
        "Manage player balances.",
        true,
        |c| {
            let give = CommandNode::literal("give")
                .then(players_node("player").then(
                    num_node("amount").execute(h(|s, _, a| economy::eco(s, a, EcoOp::Give))),
                ));
            let take = CommandNode::literal("take")
                .then(players_node("player").then(
                    num_node("amount").execute(h(|s, _, a| economy::eco(s, a, EcoOp::Take))),
                ));
            let set = CommandNode::literal("set").then(
                players_node("player")
                    .then(num_node("amount").execute(h(|s, _, a| economy::eco(s, a, EcoOp::Set)))),
            );
            let reset = CommandNode::literal("reset").then(
                players_node("player").execute(h(|s, _, a| economy::eco(s, a, EcoOp::Reset))),
            );
            c.then(give).then(take).then(set).then(reset)
        },
    );

    // ---- admin -----------------------------------------------------------
    reg(
        ctx,
        "essentials",
        &["essentials", "eessentials", "ess", "eess", "essversion"],
        "Plugin info and reload.",
        true,
        |c| {
            c.execute(h(|s, _, _| {
                say(
                    s,
                    &format!("essentials-pumpkin v{}", env!("CARGO_PKG_VERSION")),
                );
                ok()
            }))
            .then(CommandNode::literal("reload").execute(h(|s, _, _| {
                with(|st| st.load());
                say(s, "Configuration and data reloaded.");
                ok()
            })))
        },
    );
}
