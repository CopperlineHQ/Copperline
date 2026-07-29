#include "floppybridge_abstract.h"
#include "ADFBridge.h"
#include "RotationExtractor.h"
#include <windows.h>


/*****************************************************************************
 * ADFBridge - THIS IS NOT MEANT TO BE USED IN PRODUCTION
 * THIS IS PURELY FOR TESTING THE 'BRIDGE' INTERFACE WITH A KNOWN WORKING FILE
 ****************************************************************************/


#define MFM_MASK    0x55555555L		
#define AMIGA_WORD_SYNC  0x4489							 // Disk SYNC code for the Amiga start of sector
#define SECTOR_BYTES	512								 // Number of bytes in a decoded sector
#define NUM_SECTORS_PER_TRACK 11						 // Number of sectors per track
#define RAW_SECTOR_SIZE (8+56+SECTOR_BYTES+SECTOR_BYTES)      // Size of a sector, *Including* the sector sync word longs
#define ADF_TRACK_SIZE (SECTOR_BYTES*NUM_SECTORS_PER_TRACK)   // Bytes required for a single track

typedef unsigned char RawEncodedSector[RAW_SECTOR_SIZE];
typedef unsigned char RawDecodedSector[SECTOR_BYTES];
typedef RawDecodedSector RawDecodedTrack[NUM_SECTORS_PER_TRACK];
typedef unsigned char RawMFMData[SECTOR_BYTES + SECTOR_BYTES];


typedef struct alignas(1) {
	unsigned char filler1[4];  // Padding at the start of the track.  This will be set to 0xaa
	// Raw sector data
	RawEncodedSector sectors[NUM_SECTORS_PER_TRACK];   // 11968 bytes
	// Blank "Filler" gap content. (this may get overwritten by the sectors a little)
	unsigned char filler2[696];
} FullDiskTrack;
typedef struct alignas(8) {
	unsigned char trackFormat;        // This will be 0xFF for Amiga
	unsigned char trackNumber;        // Current track number (this is actually (tracknumber*2) + side
	unsigned char sectorNumber;       // The sector we just read (0 to 11)
	unsigned char sectorsRemaining;   // How many more sectors remain until the gap (0 to 10)

	unsigned long sectorLabel[4];     // OS Recovery Data, we ignore this

	unsigned long headerChecksum;	  // Read from the header, header checksum
	unsigned long dataChecksum;		  // Read from the header, data checksum

	unsigned long headerChecksumCalculated;   // The header checksum we calculate
	unsigned long dataChecksumCalculated;     // The data checksum we calculate

	RawDecodedSector data;          // decoded sector data

	RawMFMData rawSector;   // raw track data, for analysis of invalid sectors
} DecodedSector;
struct DecodedTrack {
	// A list of valid sectors where the checksums are OK
	std::vector<DecodedSector> validSectors;
	// A list of sectors found with invalid checksums.  These are used if ignore errors is triggered
	// We keep copies of each one so we can perform a statistical analysis to see if we can get a working one based on which bits are mostly set the same
	std::vector<DecodedSector> invalidSectors[NUM_SECTORS_PER_TRACK];
};



// MFM encoding algorithm part 1 - this just writes the actual data bits in the right places
// *input;	RAW data buffer (size == data_size) 
// *output;	MFM encoded buffer (size == data_size*2) 
// Returns the checksum calculated over the data
unsigned long encodeMFMdataPart1(const unsigned long* input, unsigned long* output, const unsigned int data_size) {
	unsigned long chksum = 0L;
	unsigned int count;

	unsigned long* outputOdd = output;
	unsigned long* outputEven = (unsigned long*)(((unsigned char*)output) + data_size);

	// Encode over two passes.  First split out the odd and even data, then encode the MFM values, the /4 is because we're working in longs, not bytes
	for (count = 0; count < data_size / 4; count++) {
		*outputEven = *input & MFM_MASK;
		*outputOdd = ((*input) >> 1) & MFM_MASK;
		outputEven++;
		outputOdd++;
		input++;
	}

	// Checksum calculator
	// Encode over two passes.  First split out the odd and even data, then encode the MFM values, the /4 is because we're working in longs, not bytes
	for (count = 0; count < (data_size / 4) * 2; count++) {
		chksum ^= *output;
		output++;
	}

	return chksum & MFM_MASK;
}


// Encode a sector into the correct format for disk
void encodeSector(const unsigned int trackNumber, const ADFBridge::DiskSurface surface, const unsigned int sectorNumber, const RawDecodedSector& input, RawEncodedSector& encodedSector, unsigned char& lastByte) {
	// Sector Start
	encodedSector[0] = (lastByte & 1) ? 0x2A : 0xAA;
	encodedSector[1] = 0xAA;
	encodedSector[2] = 0xAA;
	encodedSector[3] = 0xAA;
	// Sector Sync
	encodedSector[4] = 0x44;
	encodedSector[5] = 0x89;
	encodedSector[6] = 0x44;
	encodedSector[7] = 0x89;

	// MFM Encoded header
	DecodedSector header;
	memset(&header, 0, sizeof(header));

	header.trackFormat = 0xFF;
	header.trackNumber = (trackNumber << 1) | ((surface == ADFBridge::DiskSurface::dsUpper) ? 1 : 0);
	header.sectorNumber = sectorNumber;
	header.sectorsRemaining = NUM_SECTORS_PER_TRACK - sectorNumber;  //1..11


	header.headerChecksumCalculated = encodeMFMdataPart1((const unsigned long*)&header, (unsigned long*)&encodedSector[8], 4);
	// Then theres the 16 bytes of the volume label that isnt used anyway
	header.headerChecksumCalculated ^= encodeMFMdataPart1((const unsigned long*)&header.sectorLabel, (unsigned long*)&encodedSector[16], 16);
	// Thats 40 bytes written as everything doubles (8+4+4+16+16). - Encode the header checksum
	encodeMFMdataPart1((const unsigned long*)&header.headerChecksumCalculated, (unsigned long*)&encodedSector[48], 4);
	// And move on to the data section.  Next should be the checksum, but we cant encode that until we actually know its value!
	header.dataChecksumCalculated = encodeMFMdataPart1((const unsigned long*)&input, (unsigned long*)&encodedSector[64], SECTOR_BYTES);
	// And add the checksum
	encodeMFMdataPart1((const unsigned long*)&header.dataChecksumCalculated, (unsigned long*)&encodedSector[56], 4);

	// Now fill in the MFM clock bits
	bool lastBit = encodedSector[7] & (1 << 0);
	bool thisBit = lastBit;

	// Clock bits are bits 7, 5, 3 and 1
	// Data is 6, 4, 2, 0
	for (int count = 8; count < RAW_SECTOR_SIZE; count++) {
		for (int bit = 7; bit >= 1; bit -= 2) {
			lastBit = thisBit;
			thisBit = encodedSector[count] & (1 << (bit - 1));

			if (!(lastBit || thisBit)) {
				// Encode a 1!
				encodedSector[count] |= (1 << bit);
			}
		}
	}

	lastByte = encodedSector[RAW_SECTOR_SIZE - 1];
}

ADFBridge::ADFBridge(int flags, const bool canStall, const bool useIndex) {
	HANDLE fle = CreateFile(L"file.adf", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);

	RawDecodedTrack track;

	int surfaceIndex = 0;
	int currentTrack = 0;
	FullDiskTrack disktrack;

	memset(disktrack.filler1, 0xAA, sizeof(disktrack.filler1));  // Pad with "0"s which as an MFM encoded byte is 0xAA
	memset(disktrack.filler2, 0xAA, sizeof(disktrack.filler2));  // Pad with "0"s which as an MFM encoded byte is 0xAA

	DWORD read;
	while (ReadFile(fle, &track, sizeof(track), &read, NULL)) {
		if (read != sizeof(track)) break;
		DiskSurface surface = (surfaceIndex == 1) ? DiskSurface::dsUpper : DiskSurface::dsLower;

		unsigned char lastByte = disktrack.filler1[sizeof(disktrack.filler1) - 1];
		for (unsigned int sector = 0; sector < NUM_SECTORS_PER_TRACK; sector++)
			encodeSector(currentTrack, surface, sector, track[sector], disktrack.sectors[sector], lastByte);
		if (lastByte & 1) disktrack.filler2[0] = 0x2F; else disktrack.filler2[0] = 0xFF;
		unsigned char* buffer = (unsigned char*)&disktrack;
		memcpy_s(m_mfmRead[currentTrack][(int)surface].mfmBuffer, MFM_BUFFER_MAX_TRACK_LENGTH, &disktrack, sizeof(disktrack));
		m_mfmRead[currentTrack][(int)surface].amountReadInBits = sizeof(disktrack) * 8;
		// Goto the next surface/track
		surfaceIndex++;
		if (surfaceIndex > 1) {
			surfaceIndex = 0;
			currentTrack++;
		}
		// But there is a physical limit
		if (currentTrack > 81) break;

	}

	CloseHandle(fle);
}

ADFBridge::~ADFBridge() {

}

// Call to start the system up
bool ADFBridge::initialise() {
	return true;
}

// This is called prior to closing down, but shoudl reverse initialise
void ADFBridge::shutdown() {
}

// Returns TRUE when the last command requested has completed
bool ADFBridge::isReady()  { 
	return m_isMotorRunning && (GetTickCount()- m_turnOnTime > 800);
};


// While not doing anything else, the library should be continuously streaming the current track if the motor is on.  mfmBufferPosition is in BITS 
bool ADFBridge::getMFMBit(const int mfmPositionBits) {
	// Internally manage loops until the data is ready
	const int mfmPositionByte = mfmPositionBits >> 3;
	int mfmPositionBit = 7- (mfmPositionBits & 7);

	return (m_mfmRead[m_currentTrack][(int)m_floppySide].mfmBuffer[mfmPositionByte] & (1 << mfmPositionBit)) != 0;
}

// Set the status of the motor. 
void ADFBridge::setMotorStatus(bool side, bool turnOn) {
	if ((!m_isMotorRunning) && (turnOn)) m_turnOnTime = GetTickCount();
	m_isMotorRunning = turnOn;
}

// Return the maximum size of the internal track buffer in BITS
int ADFBridge::maxMFMBitPosition() {
	return m_mfmRead[m_currentTrack][(int)m_floppySide].amountReadInBits;
}

// This is called to switch to a different copy of the track so multiple revolutions can ve read
void ADFBridge::mfmSwitchBuffer(bool side) {
	m_floppySide = side ? DiskSurface::dsUpper : DiskSurface::dsLower;
}

// Seek to a specific track
void ADFBridge::gotoCylinder(int trackNumber, bool side) {
	m_floppySide = side ? DiskSurface::dsUpper : DiskSurface::dsLower;
	m_currentTrack = trackNumber;
}

// If we're on track 0, this is the emulator trying to seek to track -1.  We catch this as a special case.  
// Should perform the same operations as setCurrentCylinder in terms of diskchange etc but without changing the current cylinder
// Return FALSE if this is not supported by the bridge
void ADFBridge::handleNoClickStep(bool side) {
	m_floppySide = side ? DiskSurface::dsUpper : DiskSurface::dsLower;
}


// Submits a single WORD of data received during a DMA transfer to the disk buffer.  This needs to be saved.  It is usually flushed when commitWriteBuffer is called
// You should reset this buffer if side or track changes.  mfmPosition is provided purely for any index sync you may wish to do
void ADFBridge::writeShortToBuffer(bool side, unsigned int track, unsigned short mfmData, int mfmPosition) {
}

// Requests that any data received via writeShortToBuffer be saved to disk. The side and track should match against what you have been collected
// and the buffer should be reset upon completion.  You should return the new tracklength (maxMFMBitPosition) with optional padding if needed
unsigned int ADFBridge::commitWriteBuffer(bool side, unsigned int track) {
	m_floppySide = side ? DiskSurface::dsUpper : DiskSurface::dsLower;
	m_currentTrack = track;
	return maxMFMBitPosition();
}

// Return TRUE if we're at the INDEX marker
bool ADFBridge::isMFMPositionAtIndex(int mfmPositionBits) {
	return mfmPositionBits == 0;
}

bool ADFBridge::resetDrive(int trackNumber) {
	m_currentTrack = trackNumber;
	m_isMotorRunning = false;
	return true;
};

