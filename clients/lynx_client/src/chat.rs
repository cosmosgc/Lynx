//! Bounded, presentation-safe state for the online zone chat surface.

use std::collections::HashSet;
use std::collections::VecDeque;

use bevy::prelude::Resource;
use serde::Deserialize;

pub const MAX_CHAT_SCROLLBACK_MESSAGES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    pub id: String,
    pub channel_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub content: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMember {
    pub account_id: String,
    pub character_id: String,
}

#[derive(Debug, Default, Resource)]
pub struct ChatFeed {
    messages: VecDeque<ChatMessage>,
    members: Vec<ChatMember>,
    presence_truncated: bool,
    next_before: Option<String>,
    notice: Option<&'static str>,
}

impl ChatFeed {
    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &ChatMessage> {
        self.messages.iter()
    }

    pub fn members(&self) -> &[ChatMember] {
        &self.members
    }

    pub fn presence_truncated(&self) -> bool {
        self.presence_truncated
    }

    pub fn next_before(&self) -> Option<String> {
        self.next_before.clone()
    }

    pub fn notice(&self) -> Option<&'static str> {
        self.notice
    }

    pub(crate) fn receive_message(&mut self, message: ChatMessage) {
        if self
            .messages
            .iter()
            .any(|existing| existing.id == message.id)
        {
            return;
        }
        self.messages.push_back(message);
        while self.messages.len() > MAX_CHAT_SCROLLBACK_MESSAGES {
            self.messages.pop_front();
        }
    }

    pub(crate) fn receive_history(
        &mut self,
        messages: Vec<ChatMessage>,
        next_before: Option<String>,
        older: bool,
    ) {
        let mut merged = Vec::with_capacity(self.messages.len() + messages.len());
        if older {
            merged.extend(messages);
            merged.extend(self.messages.drain(..));
        } else {
            merged.extend(self.messages.drain(..));
            merged.extend(messages);
        }
        let mut seen = HashSet::new();
        merged.retain(|message| seen.insert(message.id.clone()));
        self.messages.extend(merged);
        while self.messages.len() > MAX_CHAT_SCROLLBACK_MESSAGES {
            if older {
                self.messages.pop_back();
            } else {
                self.messages.pop_front();
            }
        }
        self.next_before = next_before;
    }

    pub(crate) fn receive_presence(&mut self, members: Vec<ChatMember>, truncated: bool) {
        self.members = members;
        self.presence_truncated = truncated;
    }

    pub(crate) fn update_presence(&mut self, joins: Vec<ChatMember>, leaves: &[String]) {
        self.members
            .retain(|member| !leaves.contains(&member.account_id));
        for member in joins {
            if !self
                .members
                .iter()
                .any(|current| current.account_id == member.account_id)
            {
                self.members.push(member);
            }
        }
        self.members
            .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    }

    pub(crate) fn show_send_error(&mut self, code: Option<&str>) {
        self.notice = Some(match code {
            Some("rate_limited") => "Aguarde antes de enviar outra mensagem.",
            Some("capacity_exceeded") => "O chat está ocupado. Tente novamente.",
            Some("invalid_request") => "A mensagem ou o canal não é válido.",
            _ => "Não foi possível enviar a mensagem.",
        });
    }
}
