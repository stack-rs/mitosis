pub mod auth;
pub mod group;
pub mod s3;
pub mod user;

pub fn name_validator(name: &str) -> bool {
    let l = name.len();
    l > 0
        && l < 256
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}
