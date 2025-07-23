/*
 * Copyright (c) 2023 University of Oxford.
 * Copyright (c) 2023 Red Hat, Inc.
 * All rights reserved.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

 pub struct ClientStatus {
    connected: bool,
    ready: bool,
    local_checkpoint: bool,
    current_action: String, // Add this field
}

impl ClientStatus {
    pub fn new() -> Self {
        Self {
            connected: true,
            ready: false,
            local_checkpoint: false,
            current_action: String::new(), // Initialize
        }
    }
    
    // Add setter for current_action
    pub fn set_action(&mut self, action: &str) {
        self.current_action = action.to_string();
    }
    
    pub fn is_ready_for_action(&self, action: &str) -> bool {
        self.ready && self.current_action == action
    }

    // ... other methods ...
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn set_ready(&mut self, value: bool) {
        self.ready = value;
    }

    pub fn set_local_checkpoint(&mut self) {
        self.local_checkpoint = true;
    }

    pub fn has_local_checkpoint(&self) -> bool {
        self.local_checkpoint
    }
}