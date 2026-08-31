//
// Copyright 2022-2023 Stefan Reinauer
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this
//    list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//

#ifndef ROMFILE_H
#define ROMFILE_H 1

#include <exec/types.h>

/*
 * Demand-load a filesystem matching one of the given DosTypes from the
 * controller ROM or Kickstart. Called by the mounter when it finds no
 * matching FileSysEntry in FileSystem.resource. Returns nonzero if a
 * filesystem was initialized (the caller should rescan the resource).
 */
LONG LoadFileSys(ULONG id1, ULONG id2);

#endif
