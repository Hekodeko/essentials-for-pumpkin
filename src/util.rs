//! Small helpers shared by all command modules.

use pumpkin_plugin_api::{
    Player, Server,
    command::{
        Arg, ArgumentType, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType,
    },
    command_wit::Number,
    commands::CommandHandler,
    common::NamedColor,
    text::TextComponent,
    uuid::{self, Uuid},
};

use crate::state::Loc;

pub type Res = Result<i32, CommandError>;

pub fn ok() -> Res {
    Ok(1)
}

// ---------------------------------------------------------------- handlers

/// Wraps a closure so it can be used as a Pumpkin command handler.
#[derive(Clone)]
pub struct Handler<F>(pub F);

impl<F> CommandHandler for Handler<F>
where
    F: Fn(&CommandSender, &Server, &ConsumedArgs) -> Res + Send + Sync,
{
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Res {
        (self.0)(&sender, &server, &args)
    }
}

/// Constructor with the closure bound spelled out so type inference works.
pub fn h<F>(f: F) -> Handler<F>
where
    F: Fn(&CommandSender, &Server, &ConsumedArgs) -> Res + Send + Sync,
{
    Handler(f)
}

// ------------------------------------------------------------ command nodes

pub fn players_node(name: &str) -> CommandNode {
    CommandNode::argument(name, &ArgumentType::Players)
}
pub fn word_node(name: &str) -> CommandNode {
    CommandNode::argument(name, &ArgumentType::String(StringType::SingleWord))
}

pub fn num_node(name: &str) -> CommandNode {
    CommandNode::argument(name, &ArgumentType::Double((None, None)))
}

// -------------------------------------------------------------------- text

/// Parse `&`-style colour codes (the EssentialsX convention).
pub fn text(s: &str) -> TextComponent {
    TextComponent::from_legacy_string_with_code(s, '&')
}

pub fn colored(s: &str, c: NamedColor) -> TextComponent {
    TextComponent::text(s).color_named(c)
}

/// Normal feedback line to a command sender.
pub fn say(sender: &CommandSender, s: &str) {
    sender.send_message(colored(s, NamedColor::Gold));
}

/// Normal feedback line to a player.
pub fn tell(p: &Player, s: &str) {
    p.send_system_message(colored(s, NamedColor::Gold), false);
}

/// Message with `&` colour codes to a player.
pub fn tell_fmt(p: &Player, s: &str) {
    p.send_system_message(text(s), false);
}

pub fn broadcast(server: &Server, s: &str) {
    for p in server.get_all_players() {
        p.send_system_message(text(s), false);
    }
}

pub fn fail<T>(s: &str) -> Result<T, CommandError> {
    Err(CommandError::CommandFailed(colored(s, NamedColor::Red)))
}

// -------------------------------------------------------------------- args

pub fn arg_str(args: &ConsumedArgs, key: &str) -> Option<String> {
    match args.get_value(key) {
        Arg::Simple(s) | Arg::Msg(s) => Some(s),
        _ => None,
    }
}

/// First player matched by a `players` argument.
pub fn arg_player(args: &ConsumedArgs, key: &str) -> Option<Player> {
    match args.get_value(key) {
        Arg::Players(v) => v.into_iter().next(),
        _ => None,
    }
}

pub fn arg_f64(args: &ConsumedArgs, key: &str) -> Option<f64> {
    match args.get_value(key) {
        Arg::Num(Ok(n)) => Some(match n {
            Number::Float64(v) => v,
            Number::Float32(v) => f64::from(v),
            Number::Int32(v) => f64::from(v),
            Number::Int64(v) => v as f64,
        }),
        _ => None,
    }
}

/// The player who ran the command, or an error if it was the console.
pub fn me(sender: &CommandSender) -> Result<Player, CommandError> {
    sender.as_player().ok_or_else(|| {
        CommandError::CommandFailed(colored(
            "This command can only be used by a player.",
            NamedColor::Red,
        ))
    })
}

/// A named player argument, or fail with "player not found".
pub fn need_player(args: &ConsumedArgs, key: &str) -> Result<Player, CommandError> {
    arg_player(args, key)
        .ok_or_else(|| CommandError::CommandFailed(colored("Player not found.", NamedColor::Red)))
}

/// Optional `[player]` argument: the named player, or the sender when `key` is `None`.
pub fn target(
    sender: &CommandSender,
    args: &ConsumedArgs,
    key: Option<&str>,
) -> Result<Player, CommandError> {
    match key {
        Some(k) => need_player(args, k),
        None => me(sender),
    }
}

// ------------------------------------------------------------- players/locs

pub fn id_of(p: &Player) -> String {
    p.get_id().to_string()
}

pub fn dup(u: &Uuid) -> Uuid {
    Uuid {
        high: u.high,
        low: u.low,
    }
}

pub fn lookup(server: &Server, id: &str) -> Option<Player> {
    uuid::parse(id).and_then(|u| server.get_player_by_uuid(u))
}

pub fn loc_of(p: &Player) -> Loc {
    let (x, y, z) = p.get_position();
    Loc {
        world: p.get_world().get_name(),
        x,
        y,
        z,
        yaw: p.get_yaw(),
        pitch: p.get_pitch(),
    }
}

/// Teleport a player to a stored location. Returns `false` if that world isn't loaded.
pub fn teleport(server: &Server, p: &Player, l: &Loc) -> bool {
    match server.get_world_by_name(&l.world) {
        Some(w) => {
            p.teleport((l.x, l.y, l.z), Some(l.yaw), Some(l.pitch), w);
            true
        }
        None => false,
    }
}
