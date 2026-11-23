use super::condition::WeatherCondition;
use crate::agent_state::{AgentState, WaitKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase { Working, Background, Compacting, Permission, Question, Done, Idle, Error }

impl Phase {
    pub(crate) const ALL: [Self; 8] = [Self::Working, Self::Background, Self::Compacting, Self::Permission,
        Self::Question, Self::Done, Self::Idle, Self::Error];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Working => "작업 중", Self::Background => "백그라운드", Self::Compacting => "컨텍스트 압축",
            Self::Permission => "승인 대기", Self::Question => "답변 대기", Self::Done => "완료",
            Self::Idle => "쉬는 중", Self::Error => "오류",
        }
    }

    pub(crate) fn of(state: &AgentState, background: bool, done: bool) -> Option<Self> {
        match state {
            AgentState::Compacting => Some(Self::Compacting),
            AgentState::Working => Some(Self::Working),
            AgentState::Waiting { kind: WaitKind::Permission, .. } => Some(Self::Permission),
            AgentState::Waiting { kind: WaitKind::Question, .. } => Some(Self::Question),
            AgentState::Error { .. } => Some(Self::Error),
            _ if background => Some(Self::Background),
            AgentState::Idle | AgentState::Waiting { kind: WaitKind::Idle, .. } => {
                Some(if done { Self::Done } else { Self::Idle })
            }
            AgentState::Unknown => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Indicator {
    pub weather: Option<WeatherCondition>,
    /// Clockwise from the top; each bit selects an independent edge.
    pub edges: u8,
}

pub(crate) struct RemoteIndicator {
    pub key: u64,
    pub rect: [f32; 4],
    pub phase: Phase,
    pub done_until_ms: Option<u64>,
}
