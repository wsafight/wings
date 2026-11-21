use std::fmt;

use crate::ResourceLevel;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmError {
    ResourceExhausted(ResourceLevel),
    MailboxFull,
    RequestTableFull,
    IngressFull,
    IngressClosed,
    SchedulerClosed,
    StalePid,
    ForeignPid,
    UnknownHandler,
    InvalidHandler,
    InvalidCapability,
    InvalidArguments(&'static str),
    RequestTimedOut,
    RequestCancelled,
    ManagedHandlerFailed,
    Other(&'static str),
}

impl VmError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResourceExhausted(ResourceLevel::Vm) => "resource_exhausted at Vm account",
            Self::ResourceExhausted(ResourceLevel::Tenant) => {
                "resource_exhausted at Tenant account"
            }
            Self::ResourceExhausted(ResourceLevel::Process) => {
                "resource_exhausted at Process account"
            }
            Self::MailboxFull => "mailbox_full",
            Self::RequestTableFull => "request_table_full",
            Self::IngressFull => "ingress_full",
            Self::IngressClosed => "ingress_closed",
            Self::SchedulerClosed => "scheduler is closed",
            Self::StalePid => "stale scheduler Pid",
            Self::ForeignPid => "Pid belongs to another scheduler",
            Self::UnknownHandler => "unknown handler",
            Self::InvalidHandler => "invalid handler",
            Self::InvalidCapability => "invalid capability",
            Self::InvalidArguments(msg) => msg,
            Self::RequestTimedOut => "request timed out",
            Self::RequestCancelled => "request cancelled",
            Self::ManagedHandlerFailed => "managed actor handler failed",
            Self::Other(msg) => msg,
        }
    }
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for VmError {}

impl From<VmError> for String {
    fn from(err: VmError) -> Self {
        Self::from(err.as_str())
    }
}
