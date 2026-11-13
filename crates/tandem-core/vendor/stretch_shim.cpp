// C interface to Signalsmith Stretch (MIT, Geraint Luff / Signalsmith Audio).
// Adapted from signalsmith-stretch-rs by Colin Marc (MIT): interleaved
// buffers, plus a fixed random seed so every booth renders identically.

#include "signalsmith-stretch/signalsmith-stretch.h"
#include <cstddef>

namespace {
struct Interleaved {
    float *data;
    int channels;
    struct Channel {
        float *data;
        int channel;
        int stride;
        float &operator[](std::size_t i) { return data[i * stride + channel]; }
        const float &operator[](std::size_t i) const { return data[i * stride + channel]; }
    };
    Channel operator[](std::size_t c) { return Channel{data, (int)c, channels}; }
};
}

struct tandem_stretch {
    signalsmith::stretch::SignalsmithStretch<float> s{12345};
    int channels;
};

extern "C" {

tandem_stretch *tandem_stretch_create(int channels, float sample_rate) {
    auto *h = new tandem_stretch;
    h->channels = channels;
    h->s.presetDefault(channels, sample_rate);
    return h;
}

void tandem_stretch_destroy(tandem_stretch *h) { delete h; }

void tandem_stretch_reset(tandem_stretch *h) { h->s.reset(); }

int tandem_stretch_input_latency(tandem_stretch *h) { return h->s.inputLatency(); }

int tandem_stretch_output_latency(tandem_stretch *h) { return h->s.outputLatency(); }

void tandem_stretch_set_transpose(tandem_stretch *h, float factor) { h->s.setTransposeFactor(factor); }

void tandem_stretch_seek(tandem_stretch *h, float *input, int frames, double rate) {
    h->s.seek(Interleaved{input, h->channels}, frames, rate);
}

void tandem_stretch_process(tandem_stretch *h, float *input, int in_frames, float *output, int out_frames) {
    h->s.process(Interleaved{input, h->channels}, in_frames, Interleaved{output, h->channels}, out_frames);
}

}
