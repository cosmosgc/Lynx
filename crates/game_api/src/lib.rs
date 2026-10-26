//! Backend service interfaces and gameplay DTOs.

pub mod admin;
pub mod calendar;
pub mod character;
pub mod chat;
pub mod faction;
pub mod inventory;
pub mod service;
pub mod social;

pub use admin::{
    AdminClaims, AdminRole, AdminService, AuditPage, AuditQuery, AuditRecord, DEFAULT_AUDIT_PAGE,
    MAX_AUDIT_PAGE,
};
pub use calendar::{
    CalendarService, CalendarStore, ConflictResolutionView, EventEnrollmentView, EventSideView,
    TerritoryChangeView, WorldEventView, validate_calendar_snapshot,
};
pub use character::{Character, CharacterId, CharacterService, CharacterStatus};
pub use chat::{
    ChatChannel, ChatChannelKind, ChatMessage, ChatService, MAX_MESSAGE_CHARS, MAX_MESSAGE_PAGE,
    SendChatMessage, validate_channel_id, validate_page,
};
pub use faction::{
    FactionMemberView, FactionService, FactionStore, FactionView, TerritoryView, validate_snapshot,
};
pub use inventory::{InventoryService, InventorySlot, InventoryStore, validate_slots};
pub use service::{
    Account, AccountId, ApiError, AuthService, Backend, CreateCharacter, DeviceLogin,
    HealthService, HealthStatus, Session, SessionToken, WorldService, WorldSession, Zone,
};
pub use social::{Friend, FriendState, SocialService};
