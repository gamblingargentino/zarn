pub mod bash;
pub mod budget;
pub mod edit;
pub mod fsx;
pub mod mcp;
pub mod permissions;
pub mod read;
pub mod search;
pub mod write;

use zarn_core::tools::ToolRegistry;

pub fn builtins() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(bash::Bash::new()));
    registry.register(Box::new(edit::Edit::new()));
    registry.register(Box::new(read::Read::new()));
    registry.register(Box::new(search::Glob::new()));
    registry.register(Box::new(search::Grep::new()));
    registry.register(Box::new(write::Write::new()));
    registry
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_builtin_is_registered_under_its_own_name() {
        let registry = super::builtins();
        assert_eq!(
            registry.names(),
            vec!["bash", "edit", "glob", "grep", "read", "write"]
        );
    }
}
