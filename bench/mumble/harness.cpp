// Runs S's Mumble HrtfSpatializer (~/code/mumble, branch spatial-room) offline:
//   mumble-harness SOFA IN.f32 OUT.f32 AZIMUTH_DEG GAIN
// IN is mono f32le at 48 kHz; OUT is interleaved stereo f32le. Blocks of 480
// frames (Mumble's iFrameSize at 48 kHz). GAIN stands in for Mumble's distance
// gain so levels are comparable with Prim's (1.5 at table distance).
#include "HrtfSpatializer.h"

#include <QCoreApplication>

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <vector>

int main(int argc, char **argv) {
	QCoreApplication app(argc, argv);
	if (argc != 6) {
		std::fprintf(stderr, "usage: %s SOFA IN.f32 OUT.f32 AZ_DEG GAIN\n", argv[0]);
		return 2;
	}
	const int block = 480;
	HrtfSpatializer spatializer(48000, block);
	if (!spatializer.loadHRTF(QString::fromUtf8(argv[1]))) return 1;
	FILE *in = std::fopen(argv[2], "rb");
	std::vector< float > input;
	float sample;
	while (std::fread(&sample, sizeof sample, 1, in) == 1) input.push_back(sample);
	std::fclose(in);
	const float az   = std::atof(argv[4]) * static_cast< float >(M_PI) / 180.0f;
	const float gain = std::atof(argv[5]);
	// Mumble listener-local: +X right, +Y up, +Z forward.
	const float dx = std::sin(az), dy = 0.0f, dz = std::cos(az);
	const size_t blocks = (input.size() + block - 1) / block + 1;
	input.resize(blocks * block, 0.0f);
	std::vector< float > out(blocks * block * 2), tmp(block * 2);
	for (size_t k = 0; k < blocks; ++k) {
		spatializer.spatialize(1, input.data() + k * block, tmp.data(), block, dx, dy, dz);
		for (int i = 0; i < block * 2; ++i) out[k * block * 2 + i] = tmp[i] * gain;
	}
	FILE *o = std::fopen(argv[3], "wb");
	std::fwrite(out.data(), sizeof(float), out.size(), o);
	std::fclose(o);
	return 0;
}
