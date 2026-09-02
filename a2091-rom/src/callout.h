//
// Copyright 2022-2025 Chris Hooper
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice,
//    this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//

#ifndef _CALLOUT_H
#define _CALLOUT_H

typedef struct callout callout_t;
struct callout {
    int ticks;             /* ticks remaining */
    void (*func)(void *);  /* callout function at timeout */
    void *arg;             /* callout function argument */
    callout_t *co_next;    /* next callout in list */
    callout_t *co_prev;    /* previous callout in list */
};
extern callout_t *callout_head;
void callout_init(callout_t *c, u_int flags);
int callout_pending(callout_t *c);
void callout_reset(callout_t *c, int ticks, void (*func)(void *), void *arg);
int callout_stop(callout_t *c);
void callout_call(callout_t *c);
void callout_list(void);
void callout_run_timeouts(void);

#endif /* _CALLOUT_H */
