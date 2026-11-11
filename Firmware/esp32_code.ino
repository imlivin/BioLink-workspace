#include <Arduino.h>
#include <WiFi.h>
#include <Firebase_ESP_Client.h>
#include "addons/TokenHelper.h"
#include "addons/RTDBHelper.h"
#include "secrets.h" 

#define RXD2 34
#define TXD2 27 

FirebaseData fbdo;    
FirebaseData stream;  
FirebaseAuth auth;
FirebaseConfig config;

unsigned long lastUploadTime = 0;
const int uploadInterval = 1000; 

FirebaseJson batchJson; 
int readingCount = 0;   

bool isRecording = false;

void setup() {
  Serial.begin(115200);
  
  Serial2.setRxBufferSize(1024);
  Serial2.begin(115200, SERIAL_8N1, RXD2, TXD2);
  
  WiFi.begin(WIFI_SSID, WIFI_PASSWORD);
  while (WiFi.status() != WL_CONNECTED) {
    delay(300);
  }

  config.api_key = API_KEY;
  config.database_url = DATABASE_URL;
  auth.user.email = USER_EMAIL;
  auth.user.password = USER_PASSWORD;
  config.token_status_callback = tokenStatusCallback; 
  
  fbdo.setBSSLBufferSize(4096, 1024);
  
  Firebase.begin(&config, &auth);
  Firebase.reconnectWiFi(true);

  // 1. Force a manual read of the exercise state on boot before starting the stream
  if (Firebase.RTDB.getString(&fbdo, "/current/exercise")) {
    String initialExercise = fbdo.stringData();
    initialExercise.trim();
    initialExercise.replace("\"", "");
    
    if (initialExercise.equalsIgnoreCase("none")) {
      isRecording = false;
      Serial.println("Initial boot state: NONE. Paused.");
    } else {
      isRecording = true;
      Serial.println("Initial boot state: " + initialExercise + ". Recording...");
    }
  }

  // 2. Start the background stream on the "current" node
  if (!Firebase.RTDB.beginStream(&stream, "/current")) {
    Serial.printf("Stream setup failed: %s\n", stream.errorReason().c_str());
  }
}

void loop() {
  // 1. Process Stream Updates from the Mobile App
  if (Firebase.RTDB.readStream(&stream)) {
    if (stream.streamAvailable()) {
      String path = stream.dataPath();
      String dataType = stream.dataType();

      bool exerciseUpdated = false;
      String newExercise = "";

      // Check if the update is for the whole JSON object or just the exercise child
      if (dataType == "json") {
        FirebaseJson &json = stream.jsonObject();
        FirebaseJsonData result;
        json.get(result, "exercise");
        if (result.success) {
          newExercise = result.to<String>();
          exerciseUpdated = true;
        }
      } else if (path == "/exercise") {
        newExercise = stream.to<String>();
        exerciseUpdated = true;
      }

      // ONLY change recording state if the exercise string was actually modified (ignores timestamp updates)
      if (exerciseUpdated) {
        newExercise.trim();
        newExercise.replace("\"", "");

        if (newExercise.equalsIgnoreCase("none")) {
          isRecording = false;
          batchJson.clear(); 
          readingCount = 0;
          Serial.println("App says 'none'. Stopped recording.");
        } else {
          isRecording = true;
          Serial.println("App started exercise: " + newExercise + ". Recording...");
        }
      }
    }
  }

  // 2. Read Arduino Data (Constantly clear the buffer, but only save if recording)
  if (Serial2.available()) {
    String incomingData = Serial2.readStringUntil('\n');
    incomingData.trim();
    
    if (isRecording) {
      int c1 = incomingData.indexOf(',');
      int c2 = incomingData.indexOf(',', c1 + 1);
      int c3 = incomingData.indexOf(',', c2 + 1);
      int c4 = incomingData.indexOf(',', c3 + 1);
      int c5 = incomingData.indexOf(',', c4 + 1);
      
      if (c1 > 0 && c5 > 0) {
        FirebaseJson singleReading;
        singleReading.set("ax", incomingData.substring(0, c1).toInt());
        singleReading.set("ay", incomingData.substring(c1 + 1, c2).toInt());
        singleReading.set("az", incomingData.substring(c2 + 1, c3).toInt());
        singleReading.set("gx", incomingData.substring(c3 + 1, c4).toInt());
        singleReading.set("gy", incomingData.substring(c4 + 1, c5).toInt());
        singleReading.set("gz", incomingData.substring(c5 + 1).toInt());
        
        batchJson.set(String(readingCount), singleReading);
        readingCount++;
      }
    }
  }
  
  // 3. Upload Batch (Only executes if recording and data exists)
  if (millis() - lastUploadTime >= uploadInterval) {
    lastUploadTime = millis();
    
    if (isRecording && readingCount > 0) {
      Serial.print("Uploading batch of ");
      Serial.print(readingCount);
      Serial.print(" readings... ");
      
      if (Firebase.RTDB.setJSON(&fbdo, "/test_imu", &batchJson)) {
        Serial.println("SUCCESS");
      } else {
        Serial.print("FAILED: ");
        Serial.println(fbdo.errorReason());
      }
      
      batchJson.clear();
      readingCount = 0;
    }
  }
}