// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_KTL_INCLUDE_KTL_SPAN_H_
#define ZIRCON_KERNEL_LIB_KTL_INCLUDE_KTL_SPAN_H_

#include <span>

namespace ktl {

using std::span;

using std::as_bytes;
using std::as_writable_bytes;

using std::dynamic_extent;

}  // namespace ktl

#endif  // ZIRCON_KERNEL_LIB_KTL_INCLUDE_KTL_SPAN_H_
