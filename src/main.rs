mod commands;
mod config;
mod error;
mod jmap;
mod mcp;
mod models;
pub mod util;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use models::Output;
use std::io;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "fastmail-cli")]
#[command(version, about = "CLI for Fastmail's JMAP API", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate with Fastmail API token
    Auth {
        /// API token from Fastmail settings
        token: String,
    },

    /// List resources
    #[command(subcommand)]
    List(ListCommands),

    /// Get a specific email by ID
    Get {
        /// Email ID
        email_id: String,
    },

    /// Get all emails in a thread/conversation
    Thread {
        /// Email ID (will fetch entire thread this email belongs to)
        email_id: String,
    },

    /// Search emails with JMAP filters
    Search {
        /// Full-text search (from, to, cc, bcc, subject, body)
        #[arg(short, long)]
        text: Option<String>,

        /// Filter by From header
        #[arg(long)]
        from: Option<String>,

        /// Filter by To header
        #[arg(long)]
        to: Option<String>,

        /// Filter by Cc header
        #[arg(long)]
        cc: Option<String>,

        /// Filter by Bcc header
        #[arg(long)]
        bcc: Option<String>,

        /// Filter by Subject
        #[arg(long)]
        subject: Option<String>,

        /// Filter by body content
        #[arg(long)]
        body: Option<String>,

        /// Filter by mailbox name
        #[arg(short, long)]
        mailbox: Option<String>,

        /// Only emails with attachments
        #[arg(long)]
        has_attachment: bool,

        /// Minimum email size in bytes
        #[arg(long)]
        min_size: Option<u32>,

        /// Maximum email size in bytes
        #[arg(long)]
        max_size: Option<u32>,

        /// Emails received before date (ISO 8601, e.g., 2024-01-01)
        #[arg(long)]
        before: Option<String>,

        /// Emails received on or after date (ISO 8601, e.g., 2024-01-01)
        #[arg(long)]
        after: Option<String>,

        /// Only unread emails
        #[arg(long)]
        unread: bool,

        /// Only flagged/starred emails
        #[arg(long)]
        flagged: bool,

        /// Maximum results
        #[arg(short, long, default_value = "50")]
        limit: u32,
    },

    /// Send an email
    Send {
        /// Recipient(s), comma-separated
        #[arg(long)]
        to: String,

        /// Subject line
        #[arg(long)]
        subject: String,

        /// Email body (plain text)
        #[arg(long)]
        body: String,

        /// CC recipient(s), comma-separated
        #[arg(long)]
        cc: Option<String>,

        /// BCC recipient(s), comma-separated
        #[arg(long)]
        bcc: Option<String>,

        /// In-Reply-To message ID (for threading)
        #[arg(long)]
        reply_to: Option<String>,
    },

    /// Move email to a mailbox
    Move {
        /// Email ID
        email_id: String,

        /// Destination mailbox name
        #[arg(long)]
        to: String,
    },

    /// Mark email as spam
    Spam {
        /// Email ID
        email_id: String,

        /// Skip confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Mark email as read or unread
    MarkRead {
        /// Email ID
        email_id: String,

        /// Mark as unread instead of read
        #[arg(long)]
        unread: bool,
    },

    /// Download attachments from an email
    Download {
        /// Email ID
        email_id: String,

        /// Output directory (default: current directory)
        #[arg(short, long)]
        output: Option<String>,

        /// Output format: raw (save files) or json (extract text)
        #[arg(short, long)]
        format: Option<String>,

        /// Max size for images (e.g., 500K, 1M). Images larger than this are resized.
        #[arg(long)]
        max_size: Option<String>,
    },

    /// Reply to an email
    Reply {
        /// Email ID to reply to
        email_id: String,

        /// Reply body (plain text)
        #[arg(long)]
        body: String,

        /// Reply to all recipients
        #[arg(long)]
        all: bool,

        /// Additional CC recipient(s), comma-separated
        #[arg(long)]
        cc: Option<String>,

        /// BCC recipient(s), comma-separated
        #[arg(long)]
        bcc: Option<String>,
    },

    /// Forward an email
    Forward {
        /// Email ID to forward
        email_id: String,

        /// Recipient(s), comma-separated
        #[arg(long)]
        to: String,

        /// Message to include before forwarded content
        #[arg(long, default_value = "")]
        body: String,

        /// CC recipient(s), comma-separated
        #[arg(long)]
        cc: Option<String>,

        /// BCC recipient(s), comma-separated
        #[arg(long)]
        bcc: Option<String>,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Manage masked email addresses
    #[command(subcommand)]
    Masked(MaskedCommands),

    /// Run as MCP (Model Context Protocol) server for Claude integration
    Mcp,
}

#[derive(Subcommand)]
enum MaskedCommands {
    /// List all masked email addresses
    List,

    /// Create a new masked email address
    Create {
        /// Domain this masked email is for (e.g., https://example.com)
        #[arg(long)]
        domain: Option<String>,

        /// Description for the masked email
        #[arg(long)]
        description: Option<String>,

        /// Custom prefix for the email address (max 64 chars, a-z/0-9/underscore)
        #[arg(long)]
        prefix: Option<String>,
    },

    /// Enable a masked email address
    Enable {
        /// Masked email ID
        id: String,
    },

    /// Disable a masked email address
    Disable {
        /// Masked email ID
        id: String,
    },

    /// Delete a masked email address
    Delete {
        /// Masked email ID
        id: String,

        /// Skip confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ListCommands {
    /// List mailboxes (folders)
    Mailboxes,

    /// List emails in a mailbox
    Emails {
        /// Mailbox name (default: INBOX)
        #[arg(short, long, default_value = "INBOX")]
        mailbox: String,

        /// Maximum results
        #[arg(short, long, default_value = "50")]
        limit: u32,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Auth { token } => commands::auth(&token).await,

        Commands::List(cmd) => match cmd {
            ListCommands::Mailboxes => commands::list_mailboxes().await,
            ListCommands::Emails { mailbox, limit } => commands::list_emails(&mailbox, limit).await,
        },

        Commands::Get { email_id } => commands::get_email(&email_id).await,

        Commands::Thread { email_id } => commands::get_thread(&email_id).await,

        Commands::Search {
            text,
            from,
            to,
            cc,
            bcc,
            subject,
            body,
            mailbox,
            has_attachment,
            min_size,
            max_size,
            before,
            after,
            unread,
            flagged,
            limit,
        } => {
            commands::search(
                commands::SearchFilter {
                    text,
                    from,
                    to,
                    cc,
                    bcc,
                    subject,
                    body,
                    mailbox,
                    has_attachment,
                    min_size,
                    max_size,
                    before,
                    after,
                    unread,
                    flagged,
                },
                limit,
            )
            .await
        }

        Commands::Send {
            to,
            subject,
            body,
            cc,
            bcc,
            reply_to,
        } => {
            commands::send(
                &to,
                &subject,
                &body,
                cc.as_deref(),
                bcc.as_deref(),
                reply_to.as_deref(),
            )
            .await
        }

        Commands::Move { email_id, to } => commands::move_email(&email_id, &to).await,

        Commands::Spam { email_id, yes } => {
            if !yes {
                eprintln!("Mark email {} as spam? Use -y to confirm.", email_id);
                std::process::exit(1);
            }
            commands::mark_spam(&email_id).await
        }

        Commands::MarkRead { email_id, unread } => commands::mark_read(&email_id, !unread).await,

        Commands::Download {
            email_id,
            output,
            format,
            max_size,
        } => {
            commands::download_attachment(
                &email_id,
                output.as_deref(),
                format.as_deref(),
                max_size.as_deref(),
            )
            .await
        }

        Commands::Reply {
            email_id,
            body,
            all,
            cc,
            bcc,
        } => commands::reply(&email_id, &body, all, cc.as_deref(), bcc.as_deref()).await,

        Commands::Forward {
            email_id,
            to,
            body,
            cc,
            bcc,
        } => commands::forward(&email_id, &to, &body, cc.as_deref(), bcc.as_deref()).await,

        Commands::Completions { shell } => {
            generate(
                shell,
                &mut Cli::command(),
                "fastmail-cli",
                &mut io::stdout(),
            );
            return;
        }

        Commands::Masked(cmd) => match cmd {
            MaskedCommands::List => commands::list_masked_emails().await,
            MaskedCommands::Create {
                domain,
                description,
                prefix,
            } => {
                commands::create_masked_email(
                    domain.as_deref(),
                    description.as_deref(),
                    prefix.as_deref(),
                )
                .await
            }
            MaskedCommands::Enable { id } => commands::enable_masked_email(&id).await,
            MaskedCommands::Disable { id } => commands::disable_masked_email(&id).await,
            MaskedCommands::Delete { id, yes } => {
                if !yes {
                    eprintln!("Delete masked email {}? Use -y to confirm.", id);
                    std::process::exit(1);
                }
                commands::delete_masked_email(&id).await
            }
        },

        Commands::Mcp => mcp::run_server().await,
    };

    if let Err(e) = result {
        Output::<()>::error(e.to_string()).print();
        std::process::exit(1);
    }
}
