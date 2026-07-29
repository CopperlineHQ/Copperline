#pragma once
/*****************************************************************************
 * ADFBridge - THIS IS NOT MEANT TO BE USED IN PRODUCTION
 * THIS IS PURELY FOR TESTING THE 'BRIDGE' INTERFACE WITH A KNOWN WORKING FILE
 ****************************************************************************/



#include <thread>
#include <functional>
#include <queue>
#include <mutex>
#include <tchar.h>

#include "floppybridge_abstract.h"


#define MFM_BUFFER_MAX_TRACK_LENGTH			0x3800
#define MAX_CYLINDER_BRIDGE 82
class ADFBridge : public FloppyDiskBridge {
public:
	enum class DiskSurface { dsUpper = 1, dsLower = 0 };

private:
	struct MFMCache {
		unsigned char mfmBuffer[MFM_BUFFER_MAX_TRACK_LENGTH];
		int amountReadInBits = 1024;
	};
	// Cache of entire disk (or what we have so far)
	MFMCache m_mfmRead[MAX_CYLINDER_BRIDGE][2];
	int m_currentTrack = 0;
	bool m_isMotorRunning = false;
	DiskSurface m_floppySide = DiskSurface::dsLower;
	unsigned int m_turnOnTime = 0;
public:
	ADFBridge(int flags, const bool canStall, const bool useIndex);
	virtual ~ADFBridge();
	virtual bool initialise() override final;
	virtual void shutdown() override final;
	virtual const TCHAR* getDriveIDName() override final {
		return L"ADFBridge";
	}
	virtual DriveTypeID getDriveTypeID() override final { return DriveTypeID::dti35DD; };
	virtual unsigned int getLastErrorMessage(TCHAR* errorMessage, unsigned int length) override { return 0; };
	virtual bool isAtCylinder0() override final { return m_currentTrack == 0; }
	virtual unsigned char getMaxCylinder() override final { return MAX_CYLINDER_BRIDGE; };
	virtual bool isMotorRunning() override final { return m_isMotorRunning; };
	virtual void handleNoClickStep(bool side) override final;
	virtual bool isReady() override;
	virtual bool isDiskInDrive() override final { return true; };
	virtual bool hasDiskChanged() override final { return false; };
	virtual unsigned char getCurrentCylinderNumber() override final { return m_currentTrack; };
	virtual bool isWriteProtected() override final { return true; };
	virtual int getMFMSpeed(const int mfmPositionBits) override { return 1000; }
	virtual bool getMFMBit(const int mfmPositionBits) override;
	virtual void setMotorStatus(bool side, bool turnOn) override final;
	virtual int maxMFMBitPosition() override final;
	virtual void mfmSwitchBuffer(bool side) override final;
	virtual void gotoCylinder(int trackNumber, bool side) override final;
	virtual void writeShortToBuffer(bool side, unsigned int track, unsigned short mfmData, int mfmPosition) override final;
	virtual unsigned int commitWriteBuffer(bool side, unsigned int track) override final;
	virtual bool isMFMPositionAtIndex(int mfmPositionBits) override final;
	virtual bool resetDrive(int trackNumber) override final;
};

