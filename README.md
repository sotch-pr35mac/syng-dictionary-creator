# __Syng | 词应__ Dictionary Creator
#### Create a dictionary file for [Syng | 词应 Chinese-English Dictionary](http://syngdict.com)
##### v1.0.0
---

## __About__
This file takes the CC-CEDICT file and generates a json file with the appropriate information for Syng Dictionary to use. This projects preprocesses the CC-CEDICT file so that Syng load times are faster.

## __Result__
The resulting JSON file will have the words from the CC-CEDICT file in the following format:
```json
{
    "traditional": "你好",
    "simplified": "你好",
    "pronunciation": "nǐ hǎo",
    "definitions": "['Hi!', 'Hello!', 'How are you?']",
    "toneMarks": "[3, 3]",
    "searchablePinyin": "nihao",
    "searchablePinyinTones": "ni3hao3",
    "searchableEnglish": "['hi', 'hello', 'howareyou']"
}
```

## __Usage__
Once the resulting `cc-cedict.json` file has been built, you can move it to the Syng project directory at `/app/src/db/`. Now the next time Syng loads it will be using the version of CC-CEDICT that was just built.

## __Contributors__
- [Preston Wang-Stosur-Bassett](http://www.stosur.info)

## __License__
This software is licensed under the [GNU Public License v3](https://www.gnu.org/licenses/gpl-3.0.en.html).
The CC-CEDICT is licensed under the [Creative Commons Attribution-Share Alike 3.0 License](http://creativecommons.org/licenses/by-sa/3.0/).