// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/feedback/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/interface_request.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/sys/inspect/cpp/component.h>
#include <lib/syslog/cpp/logger.h>

#include <memory>

#include "src/developer/feedback/feedback_agent/feedback_agent.h"

int main(int argc, const char** argv) {
  syslog::SetTags({"feedback"});

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto context = sys::ComponentContext::Create();

  auto inspector = std::make_unique<sys::ComponentInspector>(context.get());
  inspect::Node& root_node = inspector->root();

  std::unique_ptr<feedback::FeedbackAgent> agent =
      feedback::FeedbackAgent::TryCreate(loop.dispatcher(), context->svc(), &root_node);
  if (!agent) {
    return EXIT_FAILURE;
  }

  agent->SpawnSystemLogRecorder();

  // fuchsia.feedback.ComponentDataRegister
  context->outgoing()->AddPublicService(
      ::fidl::InterfaceRequestHandler<fuchsia::feedback::ComponentDataRegister>(
          [&agent](::fidl::InterfaceRequest<fuchsia::feedback::ComponentDataRegister> request) {
            agent->HandleComponentDataRegisterRequest(std::move(request));
          }));

  // fuchsia.feedback.DataProvider
  context->outgoing()->AddPublicService(
      ::fidl::InterfaceRequestHandler<fuchsia::feedback::DataProvider>(
          [&agent](::fidl::InterfaceRequest<fuchsia::feedback::DataProvider> request) {
            agent->HandleDataProviderRequest(std::move(request));
          }));

  // fuchsia.feedback.DevideIdProvider
  context->outgoing()->AddPublicService(
      ::fidl::InterfaceRequestHandler<fuchsia::feedback::DeviceIdProvider>(
          [&agent](::fidl::InterfaceRequest<fuchsia::feedback::DeviceIdProvider> request) {
            agent->HandleDeviceIdProviderRequest(std::move(request));
          }));

  loop.Run();

  return EXIT_SUCCESS;
}
