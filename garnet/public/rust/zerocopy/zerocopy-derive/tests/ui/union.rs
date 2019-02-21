// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[macro_use]
extern crate zerocopy_derive;
extern crate zerocopy;

fn main() {}

#[derive(FromBytes)]
union Foo {}

#[derive(Unaligned)]
union Bar {}
