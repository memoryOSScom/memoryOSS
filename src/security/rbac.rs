// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use crate::config::Role;

pub fn can_store(role: Role) -> bool {
    matches!(role, Role::Writer | Role::Admin)
}

pub fn can_recall(role: Role) -> bool {
    matches!(role, Role::Reader | Role::Writer | Role::Admin)
}

pub fn can_update(role: Role) -> bool {
    matches!(role, Role::Writer | Role::Admin)
}

pub fn can_forget(role: Role) -> bool {
    matches!(role, Role::Admin)
}

pub fn can_admin(role: Role) -> bool {
    matches!(role, Role::Admin)
}
