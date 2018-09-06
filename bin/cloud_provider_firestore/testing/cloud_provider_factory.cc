// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "peridot/bin/cloud_provider_firestore/testing/cloud_provider_factory.h"

#include <utility>

#include <fuchsia/net/oldhttp/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/backoff/exponential_backoff.h>
#include <lib/fit/function.h>
#include <lib/fxl/functional/make_copyable.h>
#include <lib/fxl/random/uuid.h>
#include <lib/svc/cpp/services.h>

namespace cloud_provider_firestore {

namespace http = ::fuchsia::net::oldhttp;

namespace {
constexpr char kAppUrl[] = "cloud_provider_firestore";
}  // namespace

class CloudProviderFactory::TokenProviderContainer {
 public:
  TokenProviderContainer(
      component::StartupContext* startup_context,
      async_dispatcher_t* dispatcher,
      std::unique_ptr<service_account::Credentials> credentials,
      std::string user_id,
      fidl::InterfaceRequest<fuchsia::modular::auth::TokenProvider> request)
      : startup_context_(startup_context),
        network_wrapper_(
            dispatcher, std::make_unique<backoff::ExponentialBackoff>(),
            [this] {
              return startup_context_
                  ->ConnectToEnvironmentService<http::HttpService>();
            }),
        token_provider_(&network_wrapper_, std::move(credentials),
                        std::move(user_id)),
        binding_(&token_provider_, std::move(request)) {}

  void set_on_empty(fit::closure on_empty) {
    binding_.set_error_handler(std::move(on_empty));
  }

 private:
  component::StartupContext* const startup_context_;
  network_wrapper::NetworkWrapperImpl network_wrapper_;
  service_account::ServiceAccountTokenProvider token_provider_;
  fidl::Binding<fuchsia::modular::auth::TokenProvider> binding_;

  FXL_DISALLOW_COPY_AND_ASSIGN(TokenProviderContainer);
};

CloudProviderFactory::CloudProviderFactory(
    component::StartupContext* startup_context, std::string server_id,
    std::string api_key,
    std::unique_ptr<service_account::Credentials> credentials)
    : startup_context_(startup_context),
      server_id_(std::move(server_id)),
      api_key_(std::move(api_key)),
      credentials_(std::move(credentials)),
      services_loop_(&kAsyncLoopConfigNoAttachToThread) {
  FXL_DCHECK(startup_context);
  FXL_DCHECK(!server_id_.empty());
  FXL_DCHECK(!api_key_.empty());
}

CloudProviderFactory::~CloudProviderFactory() { services_loop_.Shutdown(); }

void CloudProviderFactory::Init() {
  services_loop_.StartThread();
  component::Services child_services;
  fuchsia::sys::LaunchInfo launch_info;
  launch_info.url = kAppUrl;
  launch_info.directory_request = child_services.NewRequest();
  startup_context_->launcher()->CreateComponent(
      std::move(launch_info), cloud_provider_controller_.NewRequest());
  child_services.ConnectToService(cloud_provider_factory_.NewRequest());
}

void CloudProviderFactory::MakeCloudProvider(
    fidl::InterfaceRequest<cloud_provider::CloudProvider> request) {
  MakeCloudProviderWithGivenUserId(fxl::GenerateUUID(), std::move(request));
}

void CloudProviderFactory::MakeCloudProviderWithGivenUserId(
    std::string user_id,
    fidl::InterfaceRequest<cloud_provider::CloudProvider> request) {
  fuchsia::modular::auth::TokenProviderPtr token_provider;
  async::PostTask(
      services_loop_.dispatcher(),
      fxl::MakeCopyable([this, user_id = std::move(user_id),
                         request = token_provider.NewRequest()]() mutable {
        token_providers_.emplace(startup_context_, services_loop_.dispatcher(),
                                 credentials_->Clone(), user_id,
                                 std::move(request));
      }));

  cloud_provider_firestore::Config firebase_config;
  firebase_config.server_id = server_id_;
  firebase_config.api_key = api_key_;

  cloud_provider_factory_->GetCloudProvider(
      std::move(firebase_config), std::move(token_provider), std::move(request),
      [](cloud_provider::Status status) {
        if (status != cloud_provider::Status::OK) {
          FXL_LOG(ERROR) << "Failed to create a cloud provider: "
                         << fidl::ToUnderlying(status);
        }
      });
}

}  // namespace cloud_provider_firestore
