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

#ifndef SCSIMSG_H
#define SCSIMSG_H 1

#include <devices/trackdisk.h>
#include "scsipi_all.h"

typedef struct scsipi_inquiry_data scsi_inquiry_data_t;

int dev_scsi_inquiry(struct IOExtTD *tio, uint unit, scsi_inquiry_data_t *inq);
int dev_scsi_get_drivegeometry(struct IOExtTD *tio, struct DriveGeometry *geom);

#endif
