//! The two message channels between the UI thread and the background agent
//! thread. The UI never blocks on network/browser work — it sends an
//! [`AgentCommand`] and reacts to the [`AgentEvent`]s that come back.

/// Sent from the UI to the agent.
#[derive(Debug, Clone)]
pub enum AgentCommand {
    /// Begin (or resume) the ChatGPT connection.
    Connect,
    /// The user pasted resume text; store it and derive fields.
    SubmitResume(String),
    /// A line the user typed into the chat box — a job link or a question.
    UserMessage(String),
    /// Re-read the current browser form and fold any user-typed values into
    /// `fields.json`.
    SyncFromBrowser,
    Quit,
}

/// Sent from the agent back to the UI.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Transient status for the header/spinner (e.g. "reading page…").
    Status(String),
    /// Show the OAuth authorize link (browser was also opened automatically).
    ConnectPrompt { url: String },
    /// Connection succeeded.
    Connected { email: String, plan: String },
    /// Connected, but `resume.txt` is blank — ask the user to paste it.
    NeedResume,
    /// Connected and resume present — ready to accept a job link.
    Ready,
    /// A streamed fragment of the assistant's reply — appended live to the
    /// current assistant line so replies render token-by-token.
    AssistantChunk(String),
    /// A neutral system/info line for the transcript.
    System(String),
    /// A non-fatal error line for the transcript.
    Error(String),
    /// `fields.json` changed; `changed` lists the affected keys.
    FieldsUpdated { changed: Vec<String> },
    /// A job form was filled.
    Applied {
        url: String,
        filled: usize,
        failed: Vec<String>,
    },
    /// The agent started/stopped working (drives the spinner + input lock).
    Busy(bool),
}
