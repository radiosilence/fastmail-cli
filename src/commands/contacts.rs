use crate::carddav::CardDavClient;
use crate::config::Config;
use crate::models::Output;

/// List all contacts from all address books
pub async fn list_contacts() -> anyhow::Result<()> {
    let config = Config::load()?;
    let username = config.get_username()?;
    let app_password = config.get_app_password()?;

    let client = CardDavClient::new(username, app_password);

    let addressbooks = client.list_addressbooks().await?;
    eprintln!("Found {} address book(s)", addressbooks.len());

    let mut all_contacts = Vec::new();
    for ab in &addressbooks {
        eprintln!("Fetching from: {}", ab.name);
        let contacts = client.list_contacts(&ab.href).await?;
        all_contacts.extend(contacts);
    }

    Output::success(all_contacts).print();
    Ok(())
}

/// Search contacts by name or email
pub async fn search_contacts(query: &str) -> anyhow::Result<()> {
    let config = Config::load()?;
    let username = config.get_username()?;
    let app_password = config.get_app_password()?;

    let client = CardDavClient::new(username, app_password);
    let contacts = client.search_contacts(query).await?;

    Output::success(contacts).print();
    Ok(())
}
