// Copyright (c) 2019-present Dmitry Stepanov and Fyrox Engine contributors.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use crate::fyrox::core::reflect::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, PartialEq, Clone, Debug, Reflect)]
pub struct SelectionSettings {
    pub ignore_back_faces: bool,

    #[serde(default = "default_hull_less_object_selection_radius")]
    pub hull_less_object_selection_radius: f32,

    // Hidden because there's a separate switch in world viewer for this.
    #[reflect(hidden)]
    pub track_selection: bool,
}

fn default_hull_less_object_selection_radius() -> f32 {
    0.33
}

impl Default for SelectionSettings {
    fn default() -> Self {
        Self {
            ignore_back_faces: false,
            hull_less_object_selection_radius: default_hull_less_object_selection_radius(),
            track_selection: true,
        }
    }
}
