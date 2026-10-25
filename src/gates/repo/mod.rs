//! Gates over version control: tracked files, credentials in history, unpushed commit messages,
//! and the workflows that run on each push.

pub mod commits;
pub mod history;
pub mod hygiene;
pub mod workflows;
