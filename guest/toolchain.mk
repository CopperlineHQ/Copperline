# Shared dockerized m68k-amigaos cross-GCC setup, included by every Makefile
# under guest/. Keep the image tag here, not duplicated per directory, so a
# toolchain bump (a version pin change, a CVE fix) is a one-line edit that
# every guest build picks up together -- a stale copy in one directory would
# silently build that target with a different compiler than the rest.

IMAGE   = stefanreinauer/amiga-gcc:gcc-v16.1
DOCKER  = docker run --rm --user $(shell id -u):$(shell id -g) \
          -v "$(CURDIR):/src" -w /src $(IMAGE)
CC      = $(DOCKER) m68k-amigaos-gcc
OBJDUMP = $(DOCKER) m68k-amigaos-objdump
OBJCOPY = $(DOCKER) m68k-amigaos-objcopy
