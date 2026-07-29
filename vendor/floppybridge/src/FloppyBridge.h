/* FloppyBridge DLL for *UAE
*
* Copyright (C) 2021-2024 Robert Smith (@RobSmithDev)
* https://amiga.robsmithdev.co.uk
*
* This file is multi-licensed under the terms of the Mozilla Public
* License Version 2.0 as published by Mozilla Corporation and the
* GNU General Public License, version 2 or later, as published by the
* Free Software Foundation.
*
* MPL2: https://www.mozilla.org/en-US/MPL/2.0/
* GPL2: https://www.gnu.org/licenses/old-licenses/gpl-2.0.en.html
*
* This file, along with currently active and supported interfaces
* are maintained from by GitHub repo at
* https://github.com/RobSmithDev/FloppyDriveBridge
*/

// The following ifdef block is the standard way of creating macros which make exporting
// from a DLL simpler. All files within this DLL are compiled with the FLOPPYBRIDGE_EXPORTS
// symbol defined on the command line. This symbol should not be defined on any project
// that uses this DLL. This way any other project whose source files include this file see
// FLOPPYBRIDGE_API functions as being imported from a DLL, whereas this DLL sees symbols
// defined with this macro as being exported.


#ifdef _WIN32

#define WIN32_LEAN_AND_MEAN             // Exclude rarely-used stuff from Windows headers
// Windows Header Files
#include <windows.h>

#define CALLING_CONVENSION _cdecl
#define FLOPPYBRIDGE_API __declspec(dllexport) 

#else

#define CALLING_CONVENSION
#define FLOPPYBRIDGE_API 

#endif 

#define BRIDGEDRIVERHANDLE
#include "floppybridge_common.h"

#include "CommonBridgeTemplate.h"

#define MAX_NUM_DRIVERS     3


// Config for a bridge setup
class BridgeConfig {
private:
	// Last serialisation of the below data, made when you call toString()
	char lastSerialise[255] = { 0 };
public:
	// Which type of interface to open
	unsigned int bridgeIndex = 0;

	// The settings that will be used to open the above bridge
	FloppyBridge::BridgeMode bridgeMode = FloppyBridge::BridgeMode::bmFast;
	FloppyBridge::BridgeDensityMode bridgeDensity = FloppyBridge::BridgeDensityMode::bdmAuto;

	// This isn't saved with fromString and toString
	char profileName[128] = { 0 };

	// Not all of these settings are used.
	char comPortToUse[128] = { 0 };
	bool autoDetectComPort = true;
	FloppyBridge::DriveSelection driveCable = FloppyBridge::DriveSelection::dsDriveA;
	bool autoCache = false;
	bool smartSpeed = false;

	// Serialising into a nice string
	bool fromString(char* string);
	void toString(char** serialisedOptions);  //the pointer returned is actually lastSerialise
};



// This is private, from the outside of the DLL its just a pointer to this
struct BridgeOpened {
	/// Details about the driver
	FloppyDiskBridge::BridgeDriver* driverDetails = nullptr;

	// The bridge, when created.
	CommonBridgeTemplate* bridge = nullptr;

	// Last error/warning message received
	char lastMessage[255] = { 0 };

	// Currently active config
	BridgeConfig config;
};