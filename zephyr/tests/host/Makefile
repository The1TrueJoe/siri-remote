# Host unit test for the pure protocol decoder (no Zephyr SDK needed).
#   make -C zephyr/tests/host run
CC ?= cc
CFLAGS ?= -Wall -Wextra -O2 -I ../../include -I ../../src
proto_test: proto_test.c ../../src/proto.c
	$(CC) $(CFLAGS) -o $@ proto_test.c ../../src/proto.c
run: proto_test
	./proto_test
clean:
	rm -f proto_test
.PHONY: run clean
