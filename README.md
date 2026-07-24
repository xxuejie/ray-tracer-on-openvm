# Ray Tracer on OpenVM

This project ports the C++ ray tracer from [Ray Tracing in One Weekend](https://raytracing.github.io/books/RayTracingInOneWeekend.html) to [OpenVM](https://github.com/openvm-org/openvm) zkVM. It's based from my earlier [Ray Tracer On Jolt](https://github.com/xxuejie/ray-tracer-on-jolt) work. I'm merely testing different zkVMs to see what are their tradeoffs.

You can also refer to [this post](https://xuejie.space/2026_07_10_cpp_ray_tracer_on_jolt_zk_vm/) for my journey porting ray tracer to Jolt.

## Usage

You should have latest llvm / clang installed. Then:

```
$ git clone --recursive https://github.com/xxuejie/ray-tracer-on-openvm
$ cd ray-tracer-on-openvm
$ git submodule update --init


$ # Build and execute the ray tracer program on OpenVM:
$ make USE_FLOAT=true

$ # As ray tracer utilizes softfloats heavily, there are 3 variations:
$ # * Plain OpenVM with softfloat
$ # * OpenVM with RISC-V zfinx extension
$ # * OpenVM with RISC-V F extension
$ # Remember that you will need `make clean` before switching variations
$ make clean && make USE_FLOAT=true VM=zfinx
$ make clean && make USE_FLOAT=true VM=f
$ make clean && make USE_FLOAT=true VM=softfloat


$ # You can also try proving mode in OpenVM. Since proving takes more resource,
$ # we will try it on a tiny image. zfinx is the fastest one, I recommend to use
$ # zfinx version (or the vanilla OpenVM version).
$ make prove-openvm USE_FLOAT=true VM=zfinx IMAGE=tiny

$ # Finally, I've built a guest program profiler you can try:
$ PROFILE_FILE=profile.txt make USE_FLOAT=true VM=zfinx
$ # Please refer to the post on how to use the profiler. We are using similar steps
$ # on Jolt and OpenVM
```

Please refer to [this design doc](./docs/design.md) for more technical details.
