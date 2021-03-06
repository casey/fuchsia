// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <byteswap.h>
#include <zircon/compiler.h>

#include "amlogic-video.h"
#include "bear_h264_hashes.h"
#include "gtest/gtest.h"
#include "h264_multi_decoder.h"
#include "h264_utils.h"
#include "macros.h"
#include "pts_manager.h"
#include "src/lib/fxl/log_settings.h"
#include "test_frame_allocator.h"
#include "tests/integration/test_25fps_h264_hashes.h"
#include "tests/test_support.h"
#include "vdec1.h"
#include "video_frame_helpers.h"

class TestH264Multi {
 public:
  static void DecodeSetStream(const char* input_filename,
                              uint8_t (*input_hashes)[SHA256_DIGEST_LENGTH], const char* filename) {
    fxl::LogSettings settings;
    settings.min_log_level = -10;
    fxl::SetLogSettings(settings);
    auto video = std::make_unique<AmlogicVideo>();
    ASSERT_TRUE(video);
    TestFrameAllocator frame_allocator(video.get());

    EXPECT_EQ(ZX_OK, video->InitRegisters(TestSupport::parent_device()));
    EXPECT_EQ(ZX_OK, video->InitDecoder());

    {
      std::lock_guard<std::mutex> lock(*video->video_decoder_lock());
      video->SetDefaultInstance(std::make_unique<H264MultiDecoder>(video.get(), &frame_allocator),
                                /*hevc=*/false);
      frame_allocator.set_decoder(video->video_decoder());
    }
    // Don't use parser, because we need to be able to save and restore the read
    // and write pointers, which can't be done if the parser is using them as
    // well.
    EXPECT_EQ(ZX_OK, video->InitializeStreamBuffer(/*use_parser=*/false, 1024 * PAGE_SIZE,
                                                   /*is_secure=*/false));
    uint32_t frame_count = 0;
    std::promise<void> wait_valid;
    {
      std::lock_guard<std::mutex> lock(*video->video_decoder_lock());
      frame_allocator.SetFrameReadyNotifier([&video, &frame_count, &wait_valid, input_filename,
                                             filename,
                                             input_hashes](std::shared_ptr<VideoFrame> frame) {
        ++frame_count;
        DLOG("Got frame %d\n", frame_count);
        EXPECT_EQ(320u, frame->coded_width);
        EXPECT_EQ(320u, frame->display_width);
        bool is_bear = input_filename == std::string("video_test_data/bear.h264");
        if (is_bear) {
          EXPECT_EQ(192u, frame->coded_height);
          EXPECT_EQ(180u, frame->display_height);
        } else {
          EXPECT_EQ(240u, frame->coded_height);
          EXPECT_EQ(240u, frame->display_height);
        }
        (void)filename;
#if DUMP_VIDEO_TO_FILE
        DumpVideoFrameToFile(frame.get(), filename);
#endif
        io_buffer_cache_flush_invalidate(&frame->buffer, 0, frame->stride * frame->coded_height);
        io_buffer_cache_flush_invalidate(&frame->buffer, frame->uv_plane_offset,
                                         frame->stride * frame->coded_height / 2);

        uint8_t* buf_start = static_cast<uint8_t*>(io_buffer_virt(&frame->buffer));
        if (frame_count == 1 && is_bear) {
          // Only test a small amount to try to make the output of huge failures obvious - the rest
          // can be verified through hashes.
          constexpr uint8_t kExpectedData[] = {124, 186, 230, 247, 252, 252, 252, 252, 252, 252};
          for (uint32_t i = 0; i < std::size(kExpectedData); ++i) {
            EXPECT_EQ(kExpectedData[i], buf_start[i]) << " index " << i;
          }
        }
        if (!is_bear || (frame_count < 4)) {
          // Later frames of bear.h264 seem to gradually be getting corrupted, so don't check them for now.
          // TODO(fxb/13483): Test later frames once they're fixed.
          uint8_t md[SHA256_DIGEST_LENGTH];
          HashFrame(frame.get(), md);
          EXPECT_EQ(0, memcmp(md, input_hashes[frame_count - 1], sizeof(md)))
              << "Incorrect hash for frame " << frame_count << ": " << StringifyHash(md);
        }

        video->AssertVideoDecoderLockHeld();
        video->video_decoder()->ReturnFrame(frame);
        uint32_t expected_frame_count = is_bear ? 26 : 240;
        if (frame_count == expected_frame_count) {
          wait_valid.set_value();
        }
      });

      // Initialize must happen after InitializeStreamBuffer or else it may misparse the SPS.
      EXPECT_EQ(ZX_OK, video->video_decoder()->Initialize());
    }

    auto input_h264 = TestSupport::LoadFirmwareFile(input_filename);
    ASSERT_NE(nullptr, input_h264);
    video->core()->InitializeDirectInput();
    auto nal_units = SplitNalUnits(input_h264->ptr, input_h264->size);
    for (auto& nal_unit : nal_units) {
      std::lock_guard<std::mutex> lock(*video->video_decoder_lock());
      auto multi_decoder = static_cast<H264MultiDecoder*>(video->video_decoder());
      multi_decoder->ProcessNalUnit(std::move(nal_unit));
    }

    EXPECT_EQ(std::future_status::ready,
              wait_valid.get_future().wait_for(std::chrono::seconds(10)));
    {
      std::lock_guard<std::mutex> lock(*video->video_decoder_lock());
      auto multi_decoder = static_cast<H264MultiDecoder*>(video->video_decoder());
      multi_decoder->DumpStatus();
    }

    EXPECT_LE(26u, frame_count);

    video->ClearDecoderInstance();
    video.reset();
  }

  static void TestInitializeTwice() {
    auto video = std::make_unique<AmlogicVideo>();
    ASSERT_TRUE(video);
    TestFrameAllocator frame_allocator(video.get());

    EXPECT_EQ(ZX_OK, video->InitRegisters(TestSupport::parent_device()));
    EXPECT_EQ(ZX_OK, video->InitDecoder());

    {
      std::lock_guard<std::mutex> lock(*video->video_decoder_lock());
      video->SetDefaultInstance(std::make_unique<H264MultiDecoder>(video.get(), &frame_allocator),
                                /*hevc=*/false);
      frame_allocator.set_decoder(video->video_decoder());
    }
    EXPECT_EQ(ZX_OK, video->InitializeStreamBuffer(/*use_parser=*/false, 1024 * PAGE_SIZE,
                                                   /*is_secure=*/false));
    {
      std::lock_guard<std::mutex> lock(*video->video_decoder_lock());

      EXPECT_EQ(ZX_OK, video->video_decoder()->Initialize());
      auto* decoder = static_cast<H264MultiDecoder*>(video->video_decoder());
      void* virt_base_1 = decoder->SecondaryFirmwareVirtualAddressForTesting();

      decoder->SetSwappedOut();
      EXPECT_EQ(ZX_OK, video->video_decoder()->InitializeHardware());
      EXPECT_EQ(virt_base_1, decoder->SecondaryFirmwareVirtualAddressForTesting());
    }
  }
};

TEST(H264Multi, DecodeBear) {
  TestH264Multi::DecodeSetStream("video_test_data/bear.h264", bear_h264_hashes,
                                 "/tmp/bearmultih264.yuv");
}

TEST(H264Multi, Decode25fps) {
  TestH264Multi::DecodeSetStream("video_test_data/test-25fps.h264", test_25fps_h264_hashes,
                                 "/tmp/test25fpsmultih264.yuv");
}

TEST(H264Multi, InitializeTwice) { TestH264Multi::TestInitializeTwice(); }
