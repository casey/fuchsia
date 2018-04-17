// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef GARNET_BIN_MEDIA_FFMPEG_FFMPEG_AUDIO_DECODER_H_
#define GARNET_BIN_MEDIA_FFMPEG_FFMPEG_AUDIO_DECODER_H_

#include <memory>

#include "garnet/bin/media/ffmpeg/ffmpeg_decoder_base.h"
#include "garnet/bin/media/ffmpeg/lpcm_util.h"
#include "lib/media/timeline/timeline_rate.h"

namespace media_player {

// Decoder implementation employing an ffmpeg audio decoder.
class FfmpegAudioDecoder : public FfmpegDecoderBase {
 public:
  static std::shared_ptr<Decoder> Create(AvCodecContextPtr av_codec_context);

  FfmpegAudioDecoder(AvCodecContextPtr av_codec_context);

  ~FfmpegAudioDecoder() override;

 protected:
  // FfmpegDecoderBase overrides.
  void OnNewInputPacket(const media::PacketPtr& packet) override;

  int BuildAVFrame(const AVCodecContext& av_codec_context,
                   AVFrame* av_frame,
                   media::PayloadAllocator* allocator) override;

  media::PacketPtr CreateOutputPacket(
      const AVFrame& av_frame,
      const std::shared_ptr<media::PayloadAllocator>& allocator) override;

 private:
  // Align sample buffers on 32-byte boundaries. This is the value that
  // Chromium uses and is supposed to work for all processor architectures.
  // Strangely, if we were to tell ffmpeg to use the default (by passing 0),
  // it aligns on 32 sample (not byte) boundaries.
  static const int kChannelAlign = 32;

  // For interleaving, if needed.
  std::unique_ptr<media::LpcmUtil> lpcm_util_;

  // For interleaving, if needed.
  std::unique_ptr<media::StreamType> stream_type_;

  // PTS rate from incoming packet.
  media::TimelineRate incoming_pts_rate_;
};

}  // namespace media_player

#endif  // GARNET_BIN_MEDIA_FFMPEG_FFMPEG_AUDIO_DECODER_H_
