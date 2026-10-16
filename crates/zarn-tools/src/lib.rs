pub mod bash;
pub mod budget;
pub mod edit;
pub mod fsx;
pub mod mcp;
pub mod permissions;
pub mod read;
pub mod search;
pub mod skill;
pub mod write;

use zarn_core::tools::{Tool, ToolRegistry};

pub fn builtin_list() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(bash::Bash::new()),
        Box::new(edit::Edit::new()),
        Box::new(read::Read::new()),
        Box::new(search::Glob::new()),
        Box::new(search::Grep::new()),
        Box::new(write::Write::new()),
    ]
}

pub fn builtins() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in builtin_list() {
        registry.register(tool);
    }
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
