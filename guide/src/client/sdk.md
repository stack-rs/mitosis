# Client SDK

The Mitosis project contains a SDK library (named `netmito`) that you can use to create your own client programmatically.

To use the SDK, add the following to your `Cargo.toml`:

```toml
[dependencies]
netmito = "0.1"
```

Here is a simple example of how to create a new user using the SDK:

```rust
# use netmito::client::MitoClient;
# use netmito::config::client::{ClientConfig, CreateUserArgs};
# 
# #[tokio::main]
# async fn main() {
// Create a new client configuration
let config = ClientConfig::default();
// Setup the client
let mut client = MitoClient::new(config);
// Create arguments for creating a new user
let args = CreateUserArgs {
    username: "new_user".to_string(),
    password: "new_password".to_string(),
    admin: false,
};
// Create a new user
client.create_user(args).await.unwrap();
# }
```

For more details, please refer to the [API documentation](https://docs.rs/netmito).
