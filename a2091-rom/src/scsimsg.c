//
// Copyright 2022-2025 Stefan Reinauer
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

#ifdef DEBUG_SCSIMSG
#define USE_SERIAL_OUTPUT
#endif

#include "port.h"
#include "printf.h"
#include <exec/types.h>
#include <exec/io.h>
#include <devices/scsidisk.h>
#include "scsimsg.h"

int
dev_scsi_inquiry(struct IOExtTD *tio, uint unit, scsi_inquiry_data_t *inq)
{
	struct scsipi_inquiry cmd;
	struct SCSICmd scmd;
	uint lun = unit / 10;
	BYTE ret;

#define SCSIPI_INQUIRY_LENGTH_SCSI2	 36

	memset(&cmd, 0, sizeof (cmd));
	cmd.opcode = INQUIRY;
	cmd.byte2 = lun << 5;
	cmd.length = sizeof (scsi_inquiry_data_t);

	memset(&scmd, 0, sizeof (scmd));
	scmd.scsi_Data = (UWORD *) inq;
	scmd.scsi_Length = sizeof (*inq);

	scmd.scsi_Command = (UBYTE *) &cmd;
	scmd.scsi_CmdLength = 6;

	scmd.scsi_Flags = SCSIF_READ | SCSIF_AUTOSENSE;

	scmd.scsi_SenseData = NULL;
	scmd.scsi_SenseLength = 0;

	tio->iotd_Req.io_Command = HD_SCSICMD;
	tio->iotd_Req.io_Length  = sizeof (scmd);
	tio->iotd_Req.io_Data	= &scmd;

	ret = DoIO((struct IORequest *) tio);
	tio->iotd_Req.io_Data	= NULL;

	return ret;
}

int
dev_scsi_get_drivegeometry(struct IOExtTD *tio, struct DriveGeometry *geom)
{
    tio->iotd_Req.io_Command = TD_GETGEOMETRY;
    tio->iotd_Req.io_Length  = sizeof (*geom);
    tio->iotd_Req.io_Data    = geom;
    tio->iotd_Req.io_Offset  = 0;
    tio->iotd_Req.io_Flags   = 0;

    return (DoIO((struct IORequest *) tio));
}
