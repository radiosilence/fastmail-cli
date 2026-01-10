mod commands;
mod config;
mod error;
mod jmap;
mod models;

use clap::{Parser, Subcommand};
use models::Output;
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

    /// Search emails
    Search {
        /// Search query
        query: String,

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

        Commands::Search { query, limit } => commands::search(&query, limit).await,

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
    };

    if let Err(e) = result {
        Output::<()>::error(e.to_string()).print();
        std::process::exit(1);
    }
}
