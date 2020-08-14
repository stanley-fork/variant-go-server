use actix::prelude::*;
use rand::{self, rngs::ThreadRng, Rng};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::game;
use crate::message;

macro_rules! catch {
    ($($code:tt)+) => {
        (|| Some({ $($code)+ }))()
    };
}

// TODO: separate game rooms to their own actors to deal with load

/// Server sends this when a new room is created
#[derive(Message, Clone)]
#[rtype(result = "()")]
pub enum Message {
    // TODO: Use a proper struct, not magic tuples
    AnnounceRoom(u32, String),
    CloseRoom(u32),
    GameStatus {
        room_id: u32,
        members: Vec<u64>,
        view: game::GameView,
    },
    Identify(Profile),
    UpdateProfile(Profile),
}

/// New chat session is created
#[derive(Message)]
#[rtype(usize)]
pub struct Connect {
    pub addr: Recipient<Message>,
}

/// Session is disconnected
#[derive(Message)]
#[rtype(result = "()")]
pub struct Disconnect {
    pub id: usize,
}

/// List of available rooms
pub struct ListRooms;

impl actix::Message for ListRooms {
    // TODO: Use a proper struct, not magic tuples
    type Result = Vec<(u32, String)>;
}

/// Join room, if room does not exists create new one.
#[derive(Message)]
#[rtype(result = "()")]
pub struct Join {
    /// Client id
    pub id: usize,
    pub room_id: u32,
}

/// Create room, announce to clients
pub struct CreateRoom {
    /// Client id
    pub id: usize,
    /// Room name
    pub name: String,
}

impl actix::Message for CreateRoom {
    type Result = Option<u32>;
}

#[derive(Message)]
#[rtype(result = "()")]
pub struct GameAction {
    pub id: usize,
    pub room_id: u32,
    pub action: message::GameAction,
}

#[derive(Message)]
#[rtype(Profile)]
pub struct IdentifyAs {
    pub id: usize,
    pub token: Option<String>,
    pub nick: Option<String>,
}

#[derive(Clone)]
pub struct Profile {
    pub user_id: u64,
    pub token: Uuid,
    pub nick: Option<String>,
}

pub struct Session {
    pub user_id: Option<u64>,
    pub client: Recipient<Message>,
}

pub struct Room {
    members: HashSet<usize>,
    users: HashSet<u64>,
    name: String,
    last_action: Instant,
    game: game::Game,
}

/// `ChatServer` manages chat rooms and responsible for coordinating chat
/// session. implementation is super primitive
pub struct GameServer {
    sessions: HashMap<usize, Session>,
    sessions_by_user: HashMap<u64, Vec<usize>>,
    profiles: HashMap<u64, Profile>,
    user_tokens: HashMap<Uuid, u64>,
    rooms: HashMap<u32, Room>,
    rng: ThreadRng,
    game_counter: u32,
}

impl Default for GameServer {
    fn default() -> GameServer {
        let mut rooms = HashMap::new();

        GameServer {
            sessions: HashMap::new(),
            sessions_by_user: HashMap::new(),
            profiles: HashMap::new(),
            user_tokens: HashMap::new(),
            rooms,
            rng: rand::thread_rng(),
            game_counter: 0,
        }
    }
}

impl GameServer {
    /// Send message to all users
    fn send_global_message(&self, message: Message) {
        for session in self.sessions.values() {
            let _ = session.client.do_send(message.clone());
        }
    }

    /// Send message to all users in a room
    fn send_room_message(&self, room: u32, message: Message) -> Option<()> {
        let room = self.rooms.get(&room)?;
        for user in &room.members {
            let session = self.sessions.get(&user);
            if let Some(session) = session {
                let _ = session.client.do_send(message.clone());
            }
        }
        Some(())
    }

    fn send_message(&self, session_id: usize, message: Message) {
        let session = self.sessions.get(&session_id);
        if let Some(session) = session {
            let _ = session.client.do_send(message.clone());
        }
    }

    fn send_user_message(&self, user: u64, message: Message) {
        let sessions = self.sessions_by_user.get(&user);
        if let Some(sessions) = sessions {
            for session in sessions {
                let session = self.sessions.get(&session);
                if let Some(session) = session {
                    let _ = session.client.do_send(message.clone());
                }
            }
        }
    }

    fn leave_room(&mut self, session_id: usize, room_id: u32) {
        let mut user_removed = false;

        if let Some(session) = self.sessions.get(&session_id) {
            // remove session from all rooms
            if let Some(room) = self.rooms.get_mut(&room_id) {
                if room.members.remove(&session_id) {
                    if let Some(user_id) = session.user_id {
                        let sessions = &self.sessions;
                        if !room
                            .members
                            .iter()
                            .any(|s| sessions.get(s).unwrap().user_id == Some(user_id))
                        {
                            room.users.remove(&user_id);
                            user_removed = true;
                        }
                    }
                }
            }
        }

        if user_removed {
            if let Some(room) = self.rooms.get(&room_id) {
                let msg = Message::GameStatus {
                    room_id,
                    members: room.users.iter().copied().collect(),
                    view: room.game.get_view(),
                };
                self.send_room_message(room_id, msg);
            }
        }
    }

    fn clear_timer(&self, ctx: &mut <Self as Actor>::Context) {
        // Magic number: prune games every 10 minutes
        ctx.run_interval(Duration::from_secs(60), |act, _ctx| {
            let mut killed_games = Vec::new();
            let now = Instant::now();
            for (&id, room) in &act.rooms {
                // if older than 1h
                if now - room.last_action > Duration::from_secs(60 * 60) {
                    killed_games.push(id);
                }
            }
            for id in killed_games {
                println!("Killed game: {}", id);
                act.rooms.remove(&id);
                act.send_global_message(Message::CloseRoom(id));
            }
        });
    }
}

impl Actor for GameServer {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.clear_timer(ctx);
    }
}

/// Handler for Connect message.
///
/// Register new session and assign unique id to this session
impl Handler<Connect> for GameServer {
    type Result = usize;

    fn handle(&mut self, msg: Connect, _: &mut Context<Self>) -> Self::Result {
        println!("Someone joined");

        // register session with random id
        let id = self.rng.gen::<usize>();
        self.sessions.insert(
            id,
            Session {
                user_id: None,
                client: msg.addr,
            },
        );

        // send id back
        id
    }
}

/// Handler for Disconnect message.
impl Handler<Disconnect> for GameServer {
    type Result = ();

    fn handle(&mut self, msg: Disconnect, _: &mut Context<Self>) {
        println!("Someone disconnected");

        let mut rooms = Vec::new();

        // remove session from all rooms
        for (room_id, room) in &mut self.rooms {
            if room.members.contains(&msg.id) {
                rooms.push(*room_id);
            }
        }

        for room_id in rooms {
            self.leave_room(msg.id, room_id)
        }

        // remove address
        if let Some(session) = self.sessions.remove(&msg.id) {
            if let Some(sessions) = session
                .user_id
                .and_then(|uid| self.sessions_by_user.get_mut(&uid))
            {
                sessions.retain(|&s| s != msg.id);
            }
        }
    }
}

/// Handler for `ListRooms` message.
impl Handler<ListRooms> for GameServer {
    type Result = MessageResult<ListRooms>;

    fn handle(&mut self, _: ListRooms, _: &mut Context<Self>) -> Self::Result {
        let mut rooms = Vec::new();

        for (&key, room) in &self.rooms {
            rooms.push((key, room.name.clone()));
        }

        MessageResult(rooms)
    }
}

/// Join room, send disconnect message to old room
impl Handler<Join> for GameServer {
    type Result = ();

    fn handle(&mut self, msg: Join, _: &mut Context<Self>) {
        let Join { id, room_id } = msg;

        let user_id = match catch!(self.sessions.get(&id)?.user_id?) {
            Some(x) => x,
            None => return,
        };

        let mut rooms = Vec::new();

        // remove session from all rooms
        for (room_id, room) in &mut self.rooms {
            if room.members.contains(&id) {
                rooms.push(*room_id);
            }
        }
        for room_id in rooms {
            self.leave_room(msg.id, room_id)
        }

        catch! {
            let room = self.rooms.get_mut(&room_id)?;
            room.members.insert(id);
            room.users.insert(user_id);
            let msg = Message::GameStatus {
                room_id,
                members: room.users.iter().copied().collect(),
                view: room.game.get_view(),
            };
            self.send_room_message(room_id, msg);

            // List room users' profiles
            let room = self.rooms.get(&room_id)?;
            for user_id in &room.users {
                catch! {
                    let profile = self.profiles.get(user_id)?;
                    self.send_message(id, Message::UpdateProfile(profile.clone()));
                };
            }

            // Announce the current user's profile to the room
            catch! {
                let profile = self.profiles.get(&user_id)?;
                self.send_room_message(room_id, Message::UpdateProfile(profile.clone()));
            };
        };
    }
}

/// Create room, announce to users
impl Handler<CreateRoom> for GameServer {
    type Result = MessageResult<CreateRoom>;

    fn handle(&mut self, msg: CreateRoom, _: &mut Context<Self>) -> Self::Result {
        let CreateRoom { id, name } = msg;

        // TODO: sanitize name
        // TODO: prevent spamming rooms (allow only one?)

        let user_id = match catch!(self.sessions.get(&id)?.user_id?) {
            Some(x) => x,
            None => return MessageResult(None),
        };

        let mut rooms = Vec::new();

        // remove session from all rooms
        for (room_id, room) in &mut self.rooms {
            if room.members.contains(&id) {
                rooms.push(*room_id);
            }
        }
        for room_id in rooms {
            self.leave_room(id, room_id)
        }

        // TODO: room ids are currently sequential as a hack for ordering..
        let room_id = self.game_counter;
        self.game_counter += 1;

        let mut room = Room {
            members: HashSet::new(),
            users: HashSet::new(),
            name: name.clone(),
            last_action: Instant::now(),
            game: game::Game::standard(),
        };
        room.members.insert(id);
        room.users.insert(user_id);

        self.send_message(
            id,
            Message::GameStatus {
                room_id,
                members: room.users.iter().copied().collect(),
                view: room.game.get_view(),
            },
        );

        self.rooms.insert(room_id, room);

        self.send_global_message(Message::AnnounceRoom(room_id, name));

        MessageResult(Some(room_id))
    }
}

impl Handler<GameAction> for GameServer {
    type Result = ();

    fn handle(&mut self, msg: GameAction, _: &mut Context<Self>) {
        let GameAction {
            id,
            room_id,
            action,
        } = msg;

        let user_id = match catch!(self.sessions.get(&id)?.user_id?) {
            Some(x) => x,
            None => return,
        };

        match self.rooms.get_mut(&room_id) {
            Some(room) => {
                room.last_action = Instant::now();
                // TODO: Handle errors in game actions - currently they fail quietly
                match action {
                    message::GameAction::Place(x, y) => {
                        let _ = room
                            .game
                            .make_action(user_id, game::ActionKind::Place(x, y));
                    }
                    message::GameAction::Pass => {
                        let _ = room.game.make_action(user_id, game::ActionKind::Pass);
                    }
                    message::GameAction::Cancel => {
                        let _ = room.game.make_action(user_id, game::ActionKind::Cancel);
                    }
                    message::GameAction::TakeSeat(seat_id) => {
                        let _ = room.game.take_seat(user_id, seat_id as _);
                    }
                    message::GameAction::LeaveSeat(seat_id) => {
                        let _ = room.game.leave_seat(user_id, seat_id as _);
                    }
                }
            }
            None => {}
        };

        match self.rooms.get(&room_id) {
            Some(room) => {
                self.send_room_message(
                    room_id,
                    Message::GameStatus {
                        room_id,
                        members: room.users.iter().copied().collect(),
                        view: room.game.get_view(),
                    },
                );
            }
            None => {}
        };
    }
}

impl Handler<IdentifyAs> for GameServer {
    type Result = MessageResult<IdentifyAs>;

    fn handle(&mut self, msg: IdentifyAs, _: &mut Self::Context) -> Self::Result {
        let IdentifyAs { id, token, nick } = msg;

        let rng = &mut self.rng;

        let token = token
            .and_then(|t| Uuid::parse_str(&t).ok())
            .unwrap_or_else(|| Uuid::from_bytes(rng.gen()));
        let user_id = *self.user_tokens.entry(token).or_insert_with(|| rng.gen());

        let profile = self.profiles.entry(user_id).or_insert_with(|| Profile {
            user_id,
            token,
            nick: None,
        });

        if let Some(nick) = nick {
            // TODO: sanitize nick
            profile.nick = Some(nick);
        }

        let profile = profile.clone();

        self.send_user_message(user_id, Message::Identify(profile.clone()));

        let sessions = self
            .sessions_by_user
            .entry(user_id)
            .or_insert_with(|| Vec::new());
        sessions.push(id);

        catch! {
            self.sessions.get_mut(&id)?.user_id = Some(user_id);
        };

        // Announce profile update to rooms
        let mut rooms = Vec::new();
        for (room_id, room) in &self.rooms {
            if room.users.contains(&user_id) {
                rooms.push(*room_id);
            }
        }
        for room_id in rooms {
            self.send_room_message(room_id, Message::UpdateProfile(profile.clone()));
        }

        MessageResult(profile)
    }
}
