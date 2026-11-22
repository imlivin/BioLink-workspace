// Firebase initialization using the Firebase JS SDK (Realtime Database).
//
// 1. Create a project at https://console.firebase.google.com
// 2. Add a Web app (</>), then copy its config values below.
// 3. In the console, go to Build > Realtime Database > Create Database
//    (start in test mode for development, then tighten the security rules
//    before release). This gives you the databaseURL used below.
//
// NOTE: These config values are not secrets in the traditional sense — they
// identify your Firebase project and are safe to ship in a client app.
// Access is controlled by Realtime Database security rules, not by hiding
// this config.
import { initializeApp } from 'firebase/app';
import { getAuth } from 'firebase/auth';
import { getDatabase } from 'firebase/database';

const firebaseConfig = {
  apiKey: 'AIzaSyAqLuW5mnRPokZoGue4H3aC1KFJt1hP2cQ',
  authDomain: 'biolinkdb.firebaseapp.com',
  databaseURL: 'https://biolinkdb-default-rtdb.firebaseio.com/',
  projectId: 'biolinkdb',
  storageBucket: 'biolinkdb.firebasestorage.app',
  messagingSenderId: '478177566513',
  appId: '1:478177566513:web:0b1b4faea6fede2ce71a10',
};

const app = initializeApp(firebaseConfig);

export const db = getDatabase(app);
export const auth = getAuth(app);
